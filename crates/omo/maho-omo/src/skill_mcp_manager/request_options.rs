use super::manager::SkillMcpClientOptions;
use maho_ext_mcp::request_origin::{McpRequestOptions, mcp_request_options};
use serde_json::Value;
use std::time::Duration;

pub(super) const SDK_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn build_request_options(
    config: &Value,
    options: Option<&SkillMcpClientOptions>,
) -> (Duration, McpRequestOptions) {
    let timeout = maho_ext_mcp::skill_server::skill_request_timeout_ms(config)
        .map(|ms| Duration::from_millis(ms.max(1.0) as u64))
        .unwrap_or(SDK_REQUEST_TIMEOUT);
    let signal = options.and_then(|options| options.signal.clone());
    (timeout, mcp_request_options(None, signal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_timeout_falls_back_without_arming_an_immediate_timer() {
        for invalid in [
            json!(0),
            json!(-1),
            json!(2_147_483_648_u64),
            json!("300"),
            Value::Null,
        ] {
            assert_eq!(
                build_request_options(&json!({"timeout":invalid}), None).0,
                SDK_REQUEST_TIMEOUT
            );
        }
    }

    #[test]
    fn configured_timeout_or_sdk_default() {
        for ms in [300, 120_000, i32::MAX] {
            assert_eq!(
                build_request_options(&json!({"timeout":ms}), None).0,
                Duration::from_millis(ms as u64)
            );
        }
        assert_eq!(
            build_request_options(&json!({"timeout":0.5}), None).0,
            Duration::from_millis(1)
        );
        assert_eq!(
            build_request_options(&json!({}), None).0,
            SDK_REQUEST_TIMEOUT
        );
    }

    #[test]
    fn timeout_does_not_replace_the_caller_signal() {
        let signal = maho_ext_api::AbortSignal::default();
        let options = SkillMcpClientOptions {
            signal: Some(signal.clone()),
            ..Default::default()
        };
        let (timeout, request) = build_request_options(&json!({"timeout":5000}), Some(&options));
        assert_eq!(timeout, Duration::from_secs(5));
        signal.abort();
        assert!(request.signal.expect("caller signal").is_aborted());
        assert!(build_request_options(&json!({}), None).1.signal.is_none());
    }
}
