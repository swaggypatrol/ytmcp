//! Account identity shared with the page bridge.
//!
//! The identity key is the InnerTube `DATASYNC_ID` exactly as YouTube returns it. It is hashed
//! with a fixed domain prefix into an opaque hint (`acct:<32 hex>`, also computed by the bridge
//! from the page) and a stable cache scope (`scope-<32 hex>`). The raw id never leaves this
//! module: it is not logged, persisted, or sent to the UI.

use sha2::{Digest, Sha256};
use ytm_protocol::{AccountEpoch, AccountScope};

const IDENTITY_DOMAIN: &str = "ytm-glass-account-v1\n";

fn identity_hash(datasync_id: &str) -> String {
    let digest = Sha256::digest(format!("{IDENTITY_DOMAIN}{datasync_id}").as_bytes());
    hex::encode(&digest[..16])
}

/// Opaque identity hint compared against the bridge's `bridge.ready.accountHint`.
pub fn account_hint_for(datasync_id: &str) -> String {
    format!("acct:{}", identity_hash(datasync_id))
}

/// Stable account scope so per-account caches survive restarts of the same account.
pub fn account_scope_for(datasync_id: &str) -> AccountScope {
    AccountScope::new(format!("scope-{}", identity_hash(datasync_id)))
        .expect("hex scope is a valid id")
}

/// Per-session scope used only when the catalog identity could not be read; such a session is
/// unverified and its cache does not carry over to the next start.
pub fn derive_account_scope(account_epoch: &AccountEpoch) -> AccountScope {
    AccountScope::new(format!("scope-{account_epoch}"))
        .unwrap_or_else(|_| AccountScope::new("scope-active").expect("literal scope valid"))
}

/// Reads `responseContext.mainAppWebResponseContext.datasyncId` from an InnerTube response.
pub fn extract_datasync_id(response: &serde_json::Value) -> Option<String> {
    response
        .pointer("/responseContext/mainAppWebResponseContext/datasyncId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// Reads the signed-in identity from a `getDatasyncIdsEndpoint` body (JSON behind the `)]}'`
/// anti-XSSI prefix). Anything else, such as a signed-out or HTML response, yields `None`.
pub fn parse_datasync_ids_response(body: &str) -> Option<String> {
    let json = body.trim_start().strip_prefix(")]}'").unwrap_or(body);
    let value: serde_json::Value = serde_json::from_str(json.trim_start()).ok()?;
    extract_datasync_id(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_hint_and_scope_match_shared_contract_vector() {
        // Same vector must be produced by the bridge from ytcfg DATASYNC_ID.
        let id = "synthetic-datasync-id||";
        assert_eq!(
            account_hint_for(id),
            "acct:4f44caa240d5daf847db89a7ad3cda12"
        );
        assert_eq!(
            account_scope_for(id).as_str(),
            "scope-4f44caa240d5daf847db89a7ad3cda12"
        );
        let response = serde_json::json!({
            "responseContext": { "mainAppWebResponseContext": { "datasyncId": id } }
        });
        assert_eq!(extract_datasync_id(&response).as_deref(), Some(id));
        assert_eq!(extract_datasync_id(&serde_json::json!({})), None);
    }

    #[test]
    fn datasync_ids_endpoint_body_yields_the_page_identity() {
        // Shape of the live getDatasyncIdsEndpoint response (values synthetic).
        let id = "synthetic-datasync-id||";
        let body = format!(
            ")]}}'\n{{\"datasyncIds\":[\"{id}\"],\"responseContext\":{{\"mainAppWebResponseContext\":{{\"datasyncId\":\"{id}\",\"loggedOut\":false}},\"responseId\":\"r\"}}}}"
        );
        assert_eq!(parse_datasync_ids_response(&body).as_deref(), Some(id));
        assert_eq!(
            account_hint_for(&parse_datasync_ids_response(&body).unwrap()),
            "acct:4f44caa240d5daf847db89a7ad3cda12"
        );
        // Live InnerTube POST responses (account_menu, browse) carry no identity at all.
        let post_like = r#"{"responseContext":{"responseId":"r","visitorData":"v"}}"#;
        assert_eq!(parse_datasync_ids_response(post_like), None);
        assert_eq!(parse_datasync_ids_response("<!doctype html>"), None);
    }
}
