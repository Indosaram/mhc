use maho_chord::services::errors::*;
use maho_chord::types::ServiceError;
#[test]
fn all_remote_error_codes_round_trip_and_unknown_codes_are_rejected() -> Result<(), ServiceError> {
    for code in REMOTE_SERVICE_ERROR_CODES {
        assert!(is_remote_service_error_code(code));
        let parsed: RemoteServiceErrorCode = serde_json::from_value(serde_json::json!(code))?;
        assert_eq!(serde_json::to_value(parsed)?, serde_json::json!(code));
        let error = RemoteServiceError::new(parsed, "example");
        assert_eq!(error.code, parsed);
    }
    assert!(!is_remote_service_error_code("unknown"));
    assert!(
        serde_json::from_value::<RemoteServiceErrorCode>(serde_json::json!("unknown")).is_err()
    );
    Ok(())
}
