use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use ytm_control_api::{
    AccountHandoffGoal, AccountHandoffState, ControlError, ErrorCode, ItemPhase, ItemResult,
    OperationPhase, OperationReceipt, OperationResource, Preconditions, RetryDisposition,
    Verification, VerificationLevel, WriteAction, WriteInput,
};
use ytm_core::control::journal::{Claim, OperationKey, OperationStore};
use ytm_core::ports::{MockClock, StoragePort};
use ytm_protocol::{AccountScope, AppSettings, PlaylistId, VideoId};
use ytm_storage::{CoverCacheConfig, SqliteStorage, StoragePaths};

fn store(root: &Path) -> SqliteStorage {
    SqliteStorage::open(
        StoragePaths::for_root(root),
        Arc::new(MockClock::new(10)),
        CoverCacheConfig::production(),
    )
    .unwrap()
}

fn key(id: &str) -> OperationKey {
    OperationKey {
        principal: "fixture-harness".to_owned(),
        account_scope: Some(AccountScope::new("scope-synthetic").unwrap()),
        operation_id: id.to_owned(),
    }
}

fn input_and_receipt(key: &OperationKey, action: WriteAction) -> (WriteInput, OperationReceipt) {
    let input = WriteInput {
        operation_id: key.operation_id.clone(),
        preconditions: Preconditions {
            expected_account_epoch: None,
            expected_bridge_epoch: None,
            expected_queue_revision: None,
            expected_entity_revision: None,
        },
        action,
    };
    let receipt = OperationReceipt {
        operation_id: key.operation_id.clone(),
        account_scope: key.account_scope.clone(),
        revision: 1,
        phase: OperationPhase::Accepted,
        resource: None,
        verification: None,
        item_results: Vec::new(),
        scheduling_stopped: false,
        settled: false,
        retry_disposition: RetryDisposition::QueryOperation,
        error: None,
        removed: Vec::new(),
    };
    (input, receipt)
}

fn create_action() -> WriteAction {
    WriteAction::CreatePrivatePlaylist {
        title: "Synthetic queue".into(),
        description: Some("Temporary fixture".into()),
    }
}

fn delete_action(playlist_id: &str) -> WriteAction {
    WriteAction::DeletePlaylist {
        playlist_id: PlaylistId::new(playlist_id).unwrap(),
    }
}

#[tokio::test]
async fn claims_are_atomically_deduplicated_and_bound_to_principal_account_and_payload() {
    let dir = TempDir::new().unwrap();
    let first = store(dir.path());
    let second = store(dir.path());
    let key = key("same-name");
    let (input, initial) = input_and_receipt(&key, create_action());

    let (a, b) = tokio::join!(
        first.claim(key.clone(), input.clone(), [1; 32], initial.clone(), 10),
        second.claim(key.clone(), input.clone(), [1; 32], initial.clone(), 10)
    );
    assert!(matches!(
        (a.unwrap(), b.unwrap()),
        (Claim::New(_), Claim::Existing(_)) | (Claim::Existing(_), Claim::New(_))
    ));

    let mut different = input.clone();
    different.action = WriteAction::Pause;
    assert_eq!(
        second
            .claim(key.clone(), different, [2; 32], initial.clone(), 11)
            .await
            .unwrap(),
        Claim::Conflict
    );
    assert_eq!(second.load_request(key.clone()).await.unwrap(), Some(input));
    for other_key in [
        OperationKey {
            principal: "different-harness".into(),
            ..key.clone()
        },
        OperationKey {
            account_scope: Some(AccountScope::new("another-scope").unwrap()),
            ..key.clone()
        },
        OperationKey {
            account_scope: None,
            ..key.clone()
        },
    ] {
        assert!(second.get(other_key.clone()).await.unwrap().is_none());
        assert!(
            second
                .load_request(other_key.clone())
                .await
                .unwrap()
                .is_none()
        );
        let (input, receipt) = input_and_receipt(&other_key, create_action());
        assert!(matches!(
            second
                .claim(other_key, input, [1; 32], receipt, 11)
                .await
                .unwrap(),
            Claim::New(_)
        ));
    }

    let submitted = OperationReceipt {
        revision: 2,
        phase: OperationPhase::Submitted,
        ..initial
    };
    first
        .update(key.clone(), 1, submitted.clone(), 12)
        .await
        .unwrap();
    assert_eq!(
        second
            .update(key.clone(), 1, submitted.clone(), 12)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(second.get(key).await.unwrap(), Some(submitted));
}

#[tokio::test]
async fn latest_schema_migrates_v2_without_losing_offsets_settings_or_cache_independence() {
    let dir = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(dir.path());
    let old = SqliteStorage::open_at_schema_version(
        paths,
        Arc::new(MockClock::new(10)),
        CoverCacheConfig::production(),
        2,
    )
    .unwrap();
    let video_id = VideoId::new("synthetic-video").unwrap();
    old.set_lyrics_user_offset(&video_id, 450).await.unwrap();
    let settings = AppSettings {
        lrclib_enabled: false,
        ..AppSettings::default()
    };
    old.save_settings(settings.clone()).await.unwrap();
    drop(old);

    let storage = store(dir.path());
    assert_eq!(storage.current_schema_version().unwrap(), 7);
    assert_eq!(storage.created_backups().len(), 1);
    assert_eq!(
        storage.get_lyrics_user_offset(&video_id).await.unwrap(),
        450
    );
    assert_eq!(storage.load_settings().await.unwrap(), Some(settings));

    let key = key("cache-independent");
    let (input, receipt) = input_and_receipt(&key, create_action());
    storage
        .claim(key.clone(), input, [3; 32], receipt.clone(), 10)
        .await
        .unwrap();
    storage
        .clear_cache(key.account_scope.clone())
        .await
        .unwrap();
    storage.clear_cache(None).await.unwrap();
    assert_eq!(storage.get(key).await.unwrap(), Some(receipt));
}

fn item(input_index: u32, phase: ItemPhase, id: Option<&str>) -> ItemResult {
    ItemResult {
        input_index,
        phase,
        resource_entry_id: id.map(str::to_owned),
        error: None,
    }
}

fn add_action() -> WriteAction {
    WriteAction::AddPlaylistEntries {
        playlist_id: PlaylistId::new("synthetic-playlist").unwrap(),
        created_by_operation_id: None,
        video_ids: ["video-a", "video-b", "video-c"]
            .into_iter()
            .map(|id| VideoId::new(id).unwrap())
            .collect(),
    }
}

#[tokio::test]
async fn recovery_fixture_child() {
    let Some(dir) = std::env::var_os("YTM_JOURNAL_RECOVERY_FIXTURE") else {
        return;
    };
    let dir = Path::new(&dir);
    let storage = store(dir);
    let create_key = key("create-remote");
    let (input, initial) = input_and_receipt(&create_key, create_action());
    storage
        .claim(create_key.clone(), input, [1; 32], initial.clone(), 10)
        .await
        .unwrap();
    let sent = OperationReceipt {
        phase: OperationPhase::Submitted,
        revision: 2,
        ..initial
    };
    storage.update(create_key, 1, sent, 11).await.unwrap();
    std::fs::write(
        dir.join("synthetic-provider-create"),
        b"side effect completed",
    )
    .unwrap();

    let batch_key = key("batch");
    let (input, initial) = input_and_receipt(&batch_key, add_action());
    storage
        .claim(batch_key.clone(), input, [2; 32], initial.clone(), 12)
        .await
        .unwrap();
    let batch = OperationReceipt {
        phase: OperationPhase::Partial,
        revision: 2,
        resource: Some(OperationResource::Playlist {
            id: PlaylistId::new("synthetic-playlist").unwrap(),
        }),
        verification: Some(Verification {
            level: VerificationLevel::ReadBack,
            observed_at_ms: 14,
        }),
        item_results: vec![
            item(0, ItemPhase::Verified, Some("entry-a")),
            item(1, ItemPhase::Acknowledged, Some("entry-b")),
            item(2, ItemPhase::NotStarted, None),
        ],
        ..initial
    };
    storage.update(batch_key, 1, batch, 14).await.unwrap();

    let unsent_key = key("unsent");
    let (input, initial) = input_and_receipt(&unsent_key, add_action());
    storage
        .claim(unsent_key, input, [3; 32], initial, 15)
        .await
        .unwrap();

    let delete_key = key("delete-remote");
    let (input, initial) = input_and_receipt(&delete_key, delete_action("PL_delete_fixture"));
    storage
        .claim(delete_key.clone(), input, [4; 32], initial.clone(), 16)
        .await
        .unwrap();
    let submitted = OperationReceipt {
        phase: OperationPhase::Submitted,
        revision: 2,
        resource: Some(OperationResource::Playlist {
            id: PlaylistId::new("PL_delete_fixture").unwrap(),
        }),
        ..initial
    };
    storage
        .update(delete_key.clone(), 1, submitted.clone(), 17)
        .await
        .unwrap();
    let acknowledged = OperationReceipt {
        phase: OperationPhase::Acknowledged,
        revision: 3,
        ..submitted
    };
    storage
        .update(delete_key, 2, acknowledged, 18)
        .await
        .unwrap();

    println!("journal-fixture-ready");
    std::io::stdout().flush().unwrap();
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn process_death_recovers_maybe_sent_to_unknown_and_preserves_prepared_request() {
    let dir = TempDir::new().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--exact", "recovery_fixture_child", "--nocapture"])
        .env("YTM_JOURNAL_RECOVERY_FIXTURE", dir.path())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            if line == "journal-fixture-ready" {
                return;
            }
        }
        panic!("fixture exited before committing journal records");
    })
    .await
    .unwrap();
    assert!(dir.path().join("synthetic-provider-create").is_file());
    child.kill().await.unwrap();
    assert_eq!(child.wait().await.unwrap().signal(), Some(9));

    let storage = store(dir.path());
    storage.recover(20).await.unwrap();
    let create_key = key("create-remote");
    let unknown = storage.get(create_key.clone()).await.unwrap().unwrap();
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert!(unknown.scheduling_stopped && !unknown.settled);
    assert_eq!(unknown.revision, 3);
    assert_eq!(unknown.retry_disposition, RetryDisposition::ReconcileFirst);
    assert!(matches!(
        unknown.error,
        Some(ControlError {
            code: ErrorCode::ResultUnknown,
            ..
        })
    ));
    let (input, initial) = input_and_receipt(&create_key, create_action());
    assert_eq!(
        storage
            .claim(create_key, input, [1; 32], initial, 21)
            .await
            .unwrap(),
        Claim::Existing(unknown.clone())
    );

    let batch_key = key("batch");
    let batch = storage.get(batch_key.clone()).await.unwrap().unwrap();
    assert_eq!(batch.phase, OperationPhase::Partial);
    assert!(batch.scheduling_stopped && !batch.settled);
    assert_eq!(
        batch.item_results,
        [
            item(0, ItemPhase::Verified, Some("entry-a")),
            ItemResult {
                error: Some(ErrorCode::ResultUnknown),
                ..item(1, ItemPhase::Unknown, Some("entry-b"))
            },
            item(2, ItemPhase::NotStarted, None),
        ]
    );
    assert_eq!(batch.verification.unwrap().observed_at_ms, 14);

    let delete_key = key("delete-remote");
    let delete = storage.get(delete_key.clone()).await.unwrap().unwrap();
    assert_eq!(delete.phase, OperationPhase::Unknown);
    assert_eq!(delete.revision, 4);
    assert!(delete.scheduling_stopped && !delete.settled);
    assert_eq!(delete.retry_disposition, RetryDisposition::ReconcileFirst);
    assert_eq!(
        delete.resource,
        Some(OperationResource::Playlist {
            id: PlaylistId::new("PL_delete_fixture").unwrap()
        })
    );
    assert!(delete.verification.is_none());
    let (input, initial) = input_and_receipt(&delete_key, delete_action("PL_delete_fixture"));
    assert_eq!(
        storage.load_request(delete_key.clone()).await.unwrap(),
        Some(input.clone())
    );
    assert_eq!(
        storage
            .claim(delete_key.clone(), input, [4; 32], initial.clone(), 22)
            .await
            .unwrap(),
        Claim::Existing(delete.clone())
    );
    let (different, _) = input_and_receipt(&delete_key, delete_action("PL_other_fixture"));
    assert_eq!(
        storage
            .claim(delete_key.clone(), different, [5; 32], initial, 22)
            .await
            .unwrap(),
        Claim::Conflict
    );

    let unsent_key = key("unsent");
    let unsent = storage.get(unsent_key.clone()).await.unwrap().unwrap();
    assert_eq!(unsent.phase, OperationPhase::Accepted);
    assert!(unsent.scheduling_stopped && !unsent.settled);
    assert_eq!(
        storage
            .load_request(unsent_key.clone())
            .await
            .unwrap()
            .unwrap()
            .action,
        add_action()
    );
    storage.recover(30).await.unwrap();
    assert_eq!(storage.get(unsent_key).await.unwrap(), Some(unsent));
    assert_eq!(storage.get(delete_key).await.unwrap(), Some(delete));
    assert_eq!(
        storage.get(key("create-remote")).await.unwrap(),
        Some(unknown)
    );
}

/// A handoff that was waiting for the user when the player stopped sent nothing, so recovery
/// settles it as a clean rejection the client can follow with a new request.
#[tokio::test]
async fn restart_ends_a_waiting_account_handoff_as_settled() {
    let dir = TempDir::new().unwrap();
    let storage = store(dir.path());
    let handoff_key = key("handoff-waiting");
    let action = WriteAction::AccountHandoff {
        goal: AccountHandoffGoal::SwitchAccount,
    };
    let (input, initial) = input_and_receipt(&handoff_key, action);
    storage
        .claim(handoff_key.clone(), input, [6; 32], initial.clone(), 10)
        .await
        .unwrap();
    let waiting = OperationReceipt {
        phase: OperationPhase::Acknowledged,
        revision: 2,
        resource: Some(OperationResource::AccountHandoff {
            goal: AccountHandoffGoal::SwitchAccount,
            state: AccountHandoffState::AwaitingUser,
        }),
        ..initial
    };
    storage
        .update(handoff_key.clone(), 1, waiting.clone(), 11)
        .await
        .unwrap();

    // A wait that an earlier version already recovered as unknown.
    let stuck_key = key("handoff-unknown");
    let action = WriteAction::AccountHandoff {
        goal: AccountHandoffGoal::SignIn,
    };
    let (input, initial) = input_and_receipt(&stuck_key, action);
    storage
        .claim(stuck_key.clone(), input, [7; 32], initial.clone(), 12)
        .await
        .unwrap();
    let stuck = OperationReceipt {
        phase: OperationPhase::Unknown,
        revision: 2,
        scheduling_stopped: true,
        retry_disposition: RetryDisposition::ReconcileFirst,
        error: Some(ControlError::new(
            ErrorCode::ResultUnknown,
            RetryDisposition::ReconcileFirst,
            "operation was interrupted after it may have been sent",
        )),
        resource: Some(OperationResource::AccountHandoff {
            goal: AccountHandoffGoal::SignIn,
            state: AccountHandoffState::AwaitingUser,
        }),
        ..initial
    };
    storage
        .update(stuck_key.clone(), 1, stuck, 13)
        .await
        .unwrap();

    storage.recover(20).await.unwrap();
    let healed = storage.get(stuck_key).await.unwrap().unwrap();
    assert_eq!(healed.phase, OperationPhase::Rejected);
    assert!(healed.settled);
    assert_eq!(healed.error.unwrap().code, ErrorCode::Unavailable);
    let ended = storage.get(handoff_key).await.unwrap().unwrap();
    assert_eq!(ended.phase, OperationPhase::Rejected);
    assert!(ended.settled && ended.scheduling_stopped);
    assert_eq!(ended.retry_disposition, RetryDisposition::NewPlanRequired);
    assert_eq!(ended.resource, waiting.resource);
    let error = ended.error.unwrap();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(error.message.contains("nothing was sent"), "{error:?}");
}
