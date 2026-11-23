use std::collections::BTreeMap;
use ytm_protocol::{
    AccountEpoch, AppSettings, AppSettingsPatch, BridgeEpoch, CodecError, CommandId, CoreEpoch,
    CredentialRevision, Envelope, MAX_FRAME_BYTES, MAX_SAFE_JS_INT, MessageKind, PROTOCOL_VERSION,
    PlayerCommandArgs, PlayerCommandPayload, PlayerOp, QueryErrorCode, RequestId, SenderRole,
    SessionMaterialPayload, TrackInstanceId, UiLanguage, decode_ndjson_bytes, decode_ndjson_str,
    encode_ndjson_line,
};

#[test]
fn valid_contract_fixtures_round_trip() {
    let raw = include_str!("../../../fixtures/contracts/valid_envelopes.ndjson");
    for (idx, line) in raw.lines().enumerate() {
        let env = decode_ndjson_str(line)
            .unwrap_or_else(|e| panic!("fixture line {} failed to decode: {e}", idx + 1));
        let reencoded = encode_ndjson_line(&env).expect("re-encode failed");
        let reparsed = decode_ndjson_str(&reencoded).expect("re-parse failed");
        assert_eq!(env.kind, reparsed.kind);
        assert_eq!(env.seq, reparsed.seq);
    }
}

#[test]
fn invalid_contract_fixtures_are_rejected() {
    let raw = include_str!("../../../fixtures/contracts/invalid_envelopes.ndjson");
    for (idx, line) in raw.lines().enumerate() {
        let res = decode_ndjson_str(line);
        assert!(
            res.is_err(),
            "expected invalid fixture line {} to fail, got {:?}",
            idx + 1,
            res
        );
    }
}

#[test]
fn rejects_oversized_frame_before_json_parse() {
    let oversized = vec![b'a'; MAX_FRAME_BYTES + 1];
    let err = decode_ndjson_bytes(&oversized).expect_err("must reject >1MiB frame");
    assert!(matches!(err, CodecError::OversizedFrame(_)));
}

#[test]
fn rejects_unsupported_protocol_version() {
    let line = r#"{"protocolVersion":99,"kind":"ui.ping","seq":1,"payload":{}}"#;
    let err = decode_ndjson_str(line).expect_err("must reject bad protocolVersion");
    assert!(matches!(err, CodecError::UnsupportedProtocolVersion(99)));
}

#[test]
fn rejects_raw_newline_inside_frame() {
    let line = "{\"protocolVersion\":1,\"kind\":\"ui.ping\",\"seq\":1,\n\"payload\":{}}";
    let err = decode_ndjson_str(line).expect_err("must reject raw newline");
    assert!(matches!(err, CodecError::RawNewline));
}

#[test]
fn rejects_unsafe_js_integers_and_out_of_range_volume() {
    let unsafe_int = format!(
        r#"{{"protocolVersion":{},"kind":"ui.playerCommand","seq":1,"payload":{{"commandId":"c1","op":"seek","args":{{"positionMs":{}}}}}}}"#,
        PROTOCOL_VERSION,
        MAX_SAFE_JS_INT + 1
    );
    assert!(matches!(
        decode_ndjson_str(&unsafe_int),
        Err(CodecError::InvalidNumber(_))
    ));

    let bad_vol = r#"{"protocolVersion":1,"kind":"ui.playerCommand","seq":1,"payload":{"commandId":"c2","op":"setVolume","args":{"volume":1.25}}}"#;
    assert!(matches!(
        decode_ndjson_str(bad_vol),
        Err(CodecError::InvalidPayload { .. })
    ));
}

#[test]
fn session_material_debug_never_leaks_cookies() {
    let mut cookies = BTreeMap::new();
    cookies.insert(
        "SAPISID".to_string(),
        "super-secret-cookie-value".to_string(),
    );
    cookies.insert(
        "__Secure-3PAPISID".to_string(),
        "another-secret".to_string(),
    );
    let mat = SessionMaterialPayload {
        account_epoch: AccountEpoch::new("acct-1").unwrap(),
        credential_revision: CredentialRevision::new("cred-1").unwrap(),
        cookie_pairs: cookies,
        auth_user_index: Some(0),
        channel_label: Some("Personal".to_string()),
    };
    let dbg = format!("{mat:?}");
    assert!(!dbg.contains("super-secret-cookie-value"));
    assert!(!dbg.contains("another-secret"));
    assert!(dbg.contains("redacted: true"));
}

#[test]
fn sender_role_boundaries_are_enforced() {
    assert!(MessageKind::UiPlayerCommand.is_allowed_from(SenderRole::MainUi));
    assert!(!MessageKind::HostSessionMaterial.is_allowed_from(SenderRole::MainUi));
    assert!(!MessageKind::BridgeReady.is_allowed_from(SenderRole::MainUi));
    assert!(MessageKind::BridgePlaybackObserved.is_allowed_from(SenderRole::PlayerBridge));
    assert!(!MessageKind::UiSettingsUpdate.is_allowed_from(SenderRole::PlayerBridge));
    assert!(!MessageKind::HostSessionMaterial.is_allowed_from(SenderRole::PlayerBridge));
    assert!(MessageKind::HostSessionMaterial.is_allowed_from(SenderRole::Host));
}

#[test]
fn command_envelope_builds_and_decodes_cleanly() {
    let mut env = Envelope::new(
        MessageKind::UiPlayerCommand,
        10,
        PlayerCommandPayload {
            command_id: CommandId::new("cmd-1").unwrap(),
            op: PlayerOp::Seek,
            args: Some(PlayerCommandArgs {
                track_instance_id: Some(TrackInstanceId::new("ti-1").unwrap()),
                position_ms: Some(15_000),
                volume: None,
                muted: None,
                target: None,
                shuffle: None,
                repeat: None,
                autoplay: None,
                enqueue_position: None,
                queue_entry_id: None,
                before_queue_entry_id: None,
                queue_revision: None,
            }),
        },
    )
    .unwrap();
    env.core_epoch = Some(CoreEpoch::new("core-1").unwrap());
    env.bridge_epoch = Some(BridgeEpoch::new("bridge-1").unwrap());
    env.request_id = Some(RequestId::new("req-1").unwrap());

    let line = encode_ndjson_line(&env).unwrap();
    let decoded = decode_ndjson_str(&line).unwrap();
    let payload: PlayerCommandPayload = decoded.decode_payload().unwrap();
    assert_eq!(payload.op, PlayerOp::Seek);
    assert_eq!(payload.args.unwrap().position_ms, Some(15_000));

    let err_env = Envelope::error_response(11, None, QueryErrorCode::Busy, "queue full", true);
    let err_line = encode_ndjson_line(&err_env).unwrap();
    assert!(decode_ndjson_str(&err_line).is_ok());
}

#[test]
fn settings_patch_and_updated_reply_round_trip_with_role_and_scale_checks() {
    assert!(MessageKind::UiSettingsUpdate.is_allowed_from(SenderRole::MainUi));
    assert!(MessageKind::CoreSettingsUpdated.is_allowed_from(SenderRole::Core));
    assert!(!MessageKind::CoreSettingsUpdated.is_allowed_from(SenderRole::MainUi));

    let patch_env = Envelope::new(
        MessageKind::UiSettingsUpdate,
        12,
        AppSettingsPatch {
            ui_language: Some(UiLanguage::SimplifiedChinese),
            ..Default::default()
        },
    )
    .unwrap();
    let patch_line = encode_ndjson_line(&patch_env).unwrap();
    assert!(!patch_line.contains("reduceTransparency"));
    let decoded_patch: AppSettingsPatch = decode_ndjson_str(&patch_line)
        .unwrap()
        .decode_payload()
        .unwrap();
    assert_eq!(
        decoded_patch.ui_language,
        Some(UiLanguage::SimplifiedChinese)
    );

    let updated_env =
        Envelope::new(MessageKind::CoreSettingsUpdated, 13, AppSettings::default()).unwrap();
    let updated_line = encode_ndjson_line(&updated_env).unwrap();
    assert!(decode_ndjson_str(&updated_line).is_ok());

    let bad_scale = r#"{"protocolVersion":1,"kind":"ui.settingsUpdate","seq":14,"payload":{"uiScalePercent":50}}"#;
    assert!(matches!(
        decode_ndjson_str(bad_scale),
        Err(CodecError::InvalidPayload { .. })
    ));
}
