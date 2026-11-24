use serde_json::Value;
use ytm_control_api::{RequestFrame, ServerFrame};

#[test]
fn public_requests_reject_host_messages_and_model_supplied_authority() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../fixtures/control-v1/request-cases.json"
    ))
    .unwrap();
    for case in cases["valid"].as_array().unwrap() {
        serde_json::from_value::<RequestFrame>(case.clone()).unwrap();
    }
    for case in cases["invalid"].as_array().unwrap() {
        assert!(
            serde_json::from_value::<RequestFrame>(case.clone()).is_err(),
            "{case}"
        );
    }
}

#[test]
fn operation_and_playlist_fixtures_round_trip_without_losing_occurrences() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
        "../../../fixtures/control-v1/response-cases.json"
    ))
    .unwrap();
    for case in fixtures {
        let response: ServerFrame = serde_json::from_value(case.clone()).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), case);
    }
}

#[test]
fn local_history_edit_survives_the_socket_wire_format() {
    use ytm_control_api::{
        ControlRequest, LocalHistoryEdit, Preconditions, WriteAction, WriteInput,
    };
    let frame = RequestFrame {
        control_version: ytm_control_api::CONTROL_VERSION,
        request_id: 7,
        request: ControlRequest::Submit(WriteInput {
            operation_id: "op-history".into(),
            preconditions: Preconditions {
                expected_account_epoch: None,
                expected_bridge_epoch: None,
                expected_queue_revision: None,
                expected_entity_revision: None,
            },
            action: WriteAction::EditLocalHistory {
                edit: LocalHistoryEdit::Remove {
                    entry_ids: vec!["entry-1".into()],
                },
            },
        }),
    };
    let wire = serde_json::to_string(&frame).unwrap();
    let decoded: RequestFrame = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded, frame);
}

#[test]
fn every_new_write_action_and_read_survives_the_socket_wire_format() {
    use ytm_control_api::{
        AccountHandoffGoal, ControlRequest, DjFeedbackChange, DjFeedbackTarget, DjPreferenceStance,
        DjPreferencesInput, InterruptResumePolicy, Preconditions, VideoId, WriteAction, WriteInput,
    };
    let video = || VideoId::new("fixtureVid01").unwrap();
    let actions = [
        WriteAction::ReplaceUpcoming {
            context_token: "token".into(),
            video_ids: vec![video()],
        },
        WriteAction::InterruptWith {
            context_token: "token".into(),
            video_ids: vec![video()],
            resume_policy: InterruptResumePolicy::ResumeInterrupted,
        },
        WriteAction::SetDjFeedback {
            change: DjFeedbackChange::Record {
                target: DjFeedbackTarget::Video { video_id: video() },
                stance: DjPreferenceStance::Avoid,
                note: Some("note".into()),
            },
        },
        WriteAction::SetDjFeedback {
            change: DjFeedbackChange::Clear {
                preference_id: "preference".into(),
            },
        },
        WriteAction::AccountHandoff {
            goal: AccountHandoffGoal::SwitchAccount,
        },
        WriteAction::SetListeningIntent {
            change: ytm_control_api::ListeningIntentChange::Set {
                instruction: ytm_control_api::ListeningIntentInstruction::Vocals {
                    mode: ytm_control_api::VocalsMode::PreferInstrumental,
                },
                duration: ytm_control_api::ListeningIntentDuration::Today,
                basis: ytm_control_api::ReportedIntentBasis::UserRequest,
                expected_moment_revision: Some(2),
            },
        },
    ];
    let mut requests: Vec<ControlRequest> = actions
        .into_iter()
        .map(|action| {
            ControlRequest::Submit(WriteInput {
                operation_id: "op".into(),
                preconditions: Preconditions {
                    expected_account_epoch: None,
                    expected_bridge_epoch: None,
                    expected_queue_revision: Some(3),
                    expected_entity_revision: None,
                },
                action,
            })
        })
        .collect();
    requests.push(ControlRequest::GetDjPreferences(DjPreferencesInput {
        page: None,
    }));
    requests.push(ControlRequest::ReplayOperation(
        ytm_control_api::OperationReplayInput {
            operation_id: "op-replay".into(),
            action_hash: "a".repeat(64),
            token_was_missing: true,
            explicit_preconditions: None,
        },
    ));
    for (index, request) in requests.into_iter().enumerate() {
        let frame = RequestFrame {
            control_version: ytm_control_api::CONTROL_VERSION,
            request_id: index as u64 + 1,
            request,
        };
        let wire = serde_json::to_string(&frame).unwrap();
        let decoded: RequestFrame = serde_json::from_str(&wire).unwrap_or_else(|error| {
            panic!("frame {index} did not survive the wire format: {error}: {wire}")
        });
        assert_eq!(decoded, frame);
    }
}
