use maho_ext_compaction::deterministic_fallback::*;

#[test]
fn provider_refusal_and_ordinary_errors_do_not_authorize_checkpoint() {
    assert_eq!(classify_required_compaction_fallback_failure(SummaryFailure::Request { transient: false, refused: true, truncated: false }, "refused"), None);
    assert_eq!(classify_required_compaction_fallback_failure(SummaryFailure::Other, "missing credentials"), None);
}

#[test]
fn truncated_transient_stream_has_specific_recovery_cause() {
    assert_eq!(classify_required_compaction_fallback_failure(SummaryFailure::Request { transient: true, refused: false, truncated: true }, "truncated"), Some(RequiredCompactionFallbackFailure::UpstreamStreamTruncated));
}

#[test]
fn suppression_marker_authorizes_terminal_provider_recovery() {
    assert_eq!(classify_required_compaction_fallback_failure(SummaryFailure::Other, "senpi:no-turn-retry:provider died"), Some(RequiredCompactionFallbackFailure::SummarizationProviderFailure));
}

#[test]
fn checkpoint_retains_prepared_safe_suffix_through_real_session_reconstruction() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    let entries = [json!({"type":"message","id":"old","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"old ".repeat(500),"timestamp":0}}), json!({"type":"message","id":"keep","parentId":"old","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"continue task","timestamp":0}})];
    let mut settings = default_compaction_settings();
    settings.keep_recent_tokens = 10;
    let mut preparation = prepare_compaction(&entries, &settings, true, false).unwrap();
    preparation.first_kept_entry_id = "keep".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, Some("task"), &entries, &mut diagnostics).unwrap();
    assert_eq!(result.first_kept_entry_id, "keep");
    assert_eq!(result.details.unwrap()["retainedSuffix"], "prepared");
    assert_eq!(diagnostics.candidates_checked, 1);
}

#[test]
fn missing_boundary_rejects_without_touching_transcript() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    let entries = [json!({"type":"message","id":"one","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"task ".repeat(100),"timestamp":0}}), json!({"type":"message","id":"two","parentId":"one","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"next","timestamp":0}})];
    let mut settings = default_compaction_settings(); settings.keep_recent_tokens = 1;
    let mut preparation = prepare_compaction(&entries, &settings, true, false).unwrap();
    preparation.first_kept_entry_id = "missing".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, None, &entries, &mut diagnostics);
    assert!(result.is_none());
    assert_eq!(diagnostics.rejection_reason, Some("missing-preparation-boundary"));
}

#[test]
fn malformed_custom_envelope_is_rejected_before_provider_conversion() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    let entries = [
        json!({"type":"message","id":"keep","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"task","timestamp":0}}),
        json!({"type":"message","id":"unsafe","parentId":"keep","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"custom","customType":"test","content":"apparently safe text","display":"not a boolean","timestamp":0}}),
    ];
    let mut preparation = prepare_compaction(&entries, &default_compaction_settings(), true, false).unwrap();
    preparation.first_kept_entry_id = "keep".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    assert!(create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, None, &entries, &mut diagnostics).is_none());
    assert_eq!(diagnostics.candidate_rejections[0]["unsafeMessageRole"], "custom");
    assert_eq!(diagnostics.candidate_rejections[0]["unsafeEntryId"], "unsafe");
}

#[test]
fn rejection_diagnostic_bounds_identifiers_and_preserves_machine_fields() {
    let diagnostics = DeterministicFallbackDiagnostic {
        rejection_reason: Some("unsafe-retained-content"), candidates_checked: 2,
        candidate_rejections: vec![serde_json::json!({"firstKeptEntryId":"x".repeat(200),"unsafeEntryId":"y".repeat(200),"unsafeMessageRole":"z".repeat(60),"unsafeMessageIndex":7,"rejectionReason":"unsafe-retained-content"})],
        ..Default::default()
    };
    let rendered = format_required_compaction_fallback_rejection(&diagnostics);
    let parsed: serde_json::Value = serde_json::from_str(rendered.lines().nth(1).unwrap()).unwrap();
    assert_eq!(parsed["candidatesChecked"], 2);
    assert_eq!(parsed["candidate"]["unsafeMessageIndex"], 7);
    assert!(parsed["candidate"]["firstKeptEntryId"].as_str().unwrap().len() <= 128);
    assert!(parsed["candidate"]["unsafeEntryId"].as_str().unwrap().len() <= 128);
    assert!(parsed["candidate"]["unsafeMessageRole"].as_str().unwrap().len() <= 32);
}

#[test]
fn unsafe_diagnostics_use_projected_position_after_failed_turn_drop() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    let entries = [
        json!({"type":"message","id":"keep","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"task","timestamp":0}}),
        json!({"type":"message","id":"failed","parentId":"keep","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"assistant","content":[],"stopReason":"error","timestamp":0}}),
        json!({"type":"message","id":"unsafe","parentId":"failed","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":[{"type":"image","data":"x","mimeType":"image/png"}],"timestamp":0}}),
    ];
    let mut preparation = prepare_compaction(&entries, &default_compaction_settings(), true, false).unwrap();
    preparation.first_kept_entry_id = "keep".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    assert!(create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, None, &entries, &mut diagnostics).is_none());
    assert_eq!(diagnostics.candidate_rejections[0]["unsafeMessageIndex"],3);
    assert_eq!(diagnostics.candidate_rejections[0]["unsafeEntryId"],"unsafe");
    assert_eq!(entries[1]["message"]["stopReason"],"error");
}
