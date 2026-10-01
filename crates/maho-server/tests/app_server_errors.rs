use maho_server::app_server::errors::*;
use serde_json::json;
#[test]
fn terminal_error_wire_shapes_and_rpc_codes() {
    assert_eq!(
        serialize_codex_error_info(&CodexErrorInfo::ContextWindowExceeded),
        json!("contextWindowExceeded")
    );
    assert_eq!(
        serialize_codex_error_info(&CodexErrorInfo::HttpConnectionFailed(None)),
        json!({"httpConnectionFailed":{"httpStatusCode":null}})
    );
    assert_eq!(
        serialize_codex_error_info(&CodexErrorInfo::ResponseStreamDisconnected(Some(502))),
        json!({"responseStreamDisconnected":{"httpStatusCode":502}})
    );
    assert_eq!(
        serialize_codex_error_info(&CodexErrorInfo::ActiveTurnNotSteerable(
            NonSteerableTurnKind::Compact
        )),
        json!({"activeTurnNotSteerable":{"turnKind":"compact"}})
    );
    assert_eq!(parse_error().code, -32700);
    assert_eq!(invalid_request_error().code, -32600);
    assert_eq!(method_not_found_error("missing").code, -32601);
    assert_eq!(invalid_params_error().code, -32602);
    assert_eq!(internal_error(None).code, -32603);
    assert_eq!(overloaded_error().code, -32001);
    assert_eq!(not_initialized_error().code, -32600);
    assert_eq!(already_initialized_error().code, -32600);
    assert_eq!(experimental_capability_error("test").code, -32600);
}
