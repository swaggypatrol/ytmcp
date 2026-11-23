use std::sync::Arc;

use rusqlite::{Connection, params};
use tempfile::TempDir;
use ytm_control_api::ErrorCode;
use ytm_core::control::dj_store::DjStore;
use ytm_core::control::journal::OperationStore;
use ytm_core::ports::MockClock;
use ytm_protocol::{AccountScope, VideoId};
use ytm_storage::{CoverCacheConfig, SqliteStorage, StoragePaths};

fn open(path: &StoragePaths) -> SqliteStorage {
    SqliteStorage::open(
        path.clone(),
        Arc::new(MockClock::new(10)),
        CoverCacheConfig::production(),
    )
    .unwrap()
}

fn preference(
    id: &str,
    video: &str,
    stance: ytm_control_api::DjPreferenceStance,
    at: u64,
) -> ytm_control_api::DjPreference {
    ytm_control_api::DjPreference {
        preference_id: id.into(),
        subject: ytm_control_api::DjPreferenceSubject::Video {
            video_id: VideoId::new(video).unwrap(),
            title: None,
        },
        stance,
        note: None,
        recorded_by: "fixture-harness".into(),
        recorded_at_ms: at,
    }
}

#[tokio::test]
async fn dj_preferences_replace_per_subject_respect_the_limit_and_stay_in_their_account() {
    use ytm_control_api::DjPreferenceStance::{Avoid, Prefer};
    let root = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(root.path());
    let storage = open(&paths);
    let scope = AccountScope::new("scope-fixture").unwrap();
    let other = AccountScope::new("scope-other").unwrap();

    storage
        .put_preference(scope.clone(), preference("p1", "song-a", Prefer, 10), 2)
        .await
        .unwrap();
    // The same subject is replaced, not duplicated, and does not count twice.
    storage
        .put_preference(scope.clone(), preference("p2", "song-a", Avoid, 20), 2)
        .await
        .unwrap();
    storage
        .put_preference(scope.clone(), preference("p3", "song-b", Prefer, 30), 2)
        .await
        .unwrap();
    let full = storage
        .put_preference(scope.clone(), preference("p4", "song-c", Prefer, 40), 2)
        .await
        .unwrap_err();
    assert_eq!(full.code, ErrorCode::Conflict);
    storage
        .put_preference(other.clone(), preference("o1", "song-z", Avoid, 60), 2)
        .await
        .unwrap();

    let (listed, more) = storage
        .list_preferences(scope.clone(), None, 10)
        .await
        .unwrap();
    assert!(!more);
    assert_eq!(
        listed
            .iter()
            .map(|p| (p.preference_id.as_str(), p.stance))
            .collect::<Vec<_>>(),
        [("p3", Prefer), ("p2", Avoid)]
    );
    let (first, more) = storage
        .list_preferences(scope.clone(), None, 1)
        .await
        .unwrap();
    assert!(more);
    // Replacing the listed entry between pages does not shift the next page.
    storage
        .put_preference(scope.clone(), preference("p5", "song-b", Avoid, 70), 2)
        .await
        .unwrap();
    let (rest, more) = storage
        .list_preferences(
            scope.clone(),
            Some(ytm_core::control::dj_store::PreferenceCursor::after(
                &first[0],
            )),
            1,
        )
        .await
        .unwrap();
    assert!(!more);
    assert_eq!(rest[0].preference_id, "p2");

    assert!(
        storage
            .get_preference(other.clone(), "p2".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .delete_preference(other.clone(), "p2".into())
            .await
            .unwrap()
            .is_none()
    );
    let removed = storage
        .delete_preference(scope.clone(), "p2".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(removed.preference_id, "p2");
    drop(storage);
    let reopened = open(&paths);
    let (left, _) = reopened.list_preferences(scope, None, 10).await.unwrap();
    assert_eq!(
        left.iter()
            .map(|p| p.preference_id.as_str())
            .collect::<Vec<_>>(),
        ["p5"]
    );
    let (theirs, _) = reopened.list_preferences(other, None, 10).await.unwrap();
    assert_eq!(theirs.len(), 1);
}

/// Rows written by 0.1.0-15 and earlier: a persistent preference with the former fields, a
/// session-only skip, and journal receipts of retired DJ plan and session operations.
#[tokio::test]
async fn records_of_retired_dj_designs_neither_surface_nor_break_reads() {
    let root = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(root.path());
    let scope = AccountScope::new("scope-fixture").unwrap();
    drop(open(&paths));
    {
        let conn = Connection::open(paths.database_file()).unwrap();
        let insert = "INSERT INTO control_dj_preferences
             (account_scope, preference_id, session_id, subject_kind, subject_id,
              recorded_at_ms, preference_json)
             VALUES ('scope-fixture', ?1, ?2, 'video', ?3, ?4, ?5)";
        conn.execute(
            insert,
            params![
                "old-persistent",
                "",
                "song-a",
                10,
                r#"{"preferenceId":"old-persistent","subject":{"kind":"video","videoId":"song-a","title":null},"stance":"avoid","skipReason":null,"sessionId":null,"note":"old","recordedBy":"fixture-harness","recordedAtMs":10}"#
            ],
        )
        .unwrap();
        conn.execute(
            insert,
            params![
                "old-skip",
                "session-fixture",
                "song-b",
                20,
                r#"{"preferenceId":"old-skip","subject":{"kind":"video","videoId":"song-b","title":null},"stance":"skip","skipReason":"notNow","sessionId":"session-fixture","note":null,"recordedBy":"fixture-harness","recordedAtMs":20}"#
            ],
        )
        .unwrap();
        for (id, settled, resource) in [
            ("old-plan", 1, r#"{"kind":"djPlan","id":"plan-fixture"}"#),
            (
                "old-session",
                0,
                r#"{"kind":"djSession","id":"session-fixture"}"#,
            ),
        ] {
            let receipt = format!(
                r#"{{"operationId":"{id}","accountScope":"scope-fixture","revision":2,"phase":"submitted","resource":{resource},"verification":null,"itemResults":[],"schedulingStopped":false,"settled":false,"retryDisposition":"query_operation","error":null}}"#
            );
            conn.execute(
                "INSERT INTO control_operations (principal, account_scope, operation_id,
                  payload_hash, revision, settled, updated_at_ms, request_json, receipt_json)
                 VALUES ('fixture-harness', 'scope-fixture', ?1, zeroblob(32), 2, ?2, 10, '{}', ?3)",
                params![id, settled, receipt],
            )
            .unwrap();
        }
    }

    let storage = open(&paths);
    storage.recover(30).await.unwrap();
    assert!(
        storage
            .recent(Some(scope.clone()), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let (listed, _) = storage
        .list_preferences(scope.clone(), None, 10)
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|p| (p.preference_id.as_str(), p.note.as_deref()))
            .collect::<Vec<_>>(),
        [("old-persistent", Some("old"))]
    );
    assert!(
        storage
            .get_preference(scope.clone(), "old-skip".into())
            .await
            .unwrap()
            .is_none()
    );
}
