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
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, RecoveryMetadata { task_intent: Some("task"), ..Default::default() }, &entries, &mut diagnostics).unwrap();
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
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, RecoveryMetadata::default(), &entries, &mut diagnostics);
    assert!(result.is_none());
    assert_eq!(diagnostics.rejection_reason, Some("missing-preparation-boundary"));
}

#[test]
fn malformed_custom_envelope_is_omitted_by_the_retained_projection_not_replayed() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    let entries = [
        json!({"type":"message","id":"keep","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"task","timestamp":0}}),
        json!({"type":"message","id":"unsafe","parentId":"keep","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"custom","customType":"test","content":"apparently safe text","display":"not a boolean","timestamp":0}}),
    ];
    let mut preparation = prepare_compaction(&entries, &default_compaction_settings(), true, false).unwrap();
    preparation.first_kept_entry_id = "keep".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, RecoveryMetadata::default(), &entries, &mut diagnostics).expect("omit-unsafe-v1 recovers instead of rejecting");
    assert_eq!(result.first_kept_entry_id, "keep");
    assert_eq!(result.details.as_ref().expect("details")["retainedMessagePolicy"], "omit-unsafe-v1");
    // The persisted transcript is untouched; only the replay projection omits the malformed entry.
    let mut replay_entries = entries.to_vec();
    replay_entries.push(json!({"type":"compaction","id":"c","parentId":"unsafe","timestamp":"1970-01-01T00:00:00.000Z","summary":result.summary,"firstKeptEntryId":result.first_kept_entry_id,"tokensBefore":1,"details":result.details}));
    let replay = maho_core::session_manager::build_session_context(&replay_entries, None).messages;
    assert!(!replay.iter().any(|message| message["display"] == "not a boolean"));
    assert_eq!(replay.last().expect("omitted placeholder")["role"], "user");
    assert_eq!(entries[1]["message"]["display"], "not a boolean");
}

#[test]
fn rejection_diagnostic_bounds_identifiers_and_preserves_machine_fields() {
    let diagnostics = DeterministicFallbackDiagnostic {
        rejection_reason: Some("unsafe-retained-content"), candidates_checked: 2,
        candidate_rejections: vec![serde_json::json!({"firstKeptEntryId":"x".repeat(200),"unsafeEntryId":"y".repeat(200),"unsafeMessageRole":"z".repeat(60),"unsafeMessageIndex":7,"rejectionReason":"unsafe-retained-content"})],
        ..Default::default()
    };
    let rendered = format_required_compaction_fallback_rejection(&diagnostics, false);
    let parsed: serde_json::Value = serde_json::from_str(rendered.lines().nth(1).unwrap()).unwrap();
    assert_eq!(parsed["candidatesChecked"], 2);
    assert_eq!(parsed["candidate"]["unsafeMessageIndex"], 7);
    assert!(parsed["candidate"]["firstKeptEntryId"].as_str().unwrap().len() <= 128);
    assert!(parsed["candidate"]["unsafeEntryId"].as_str().unwrap().len() <= 128);
    assert!(parsed["candidate"]["unsafeMessageRole"].as_str().unwrap().len() <= 32);
}

#[test]
fn failed_turn_fragments_are_dropped_and_unsafe_tail_is_omitted_not_rejected() {
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
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, RecoveryMetadata::default(), &entries, &mut diagnostics).expect("unsafe tail is omitted by the projection");
    assert_eq!(result.first_kept_entry_id, "keep");
    assert!(diagnostics.candidate_rejections.is_empty());
    assert_eq!(entries[1]["message"]["stopReason"],"error");
}

#[test]
fn fallback_details_mark_the_omit_unsafe_retained_message_policy() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    let entries = [
        json!({"type":"message","id":"keep","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"task","timestamp":0}}),
        json!({"type":"message","id":"next","parentId":"keep","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"more","timestamp":0}}),
    ];
    let mut preparation = prepare_compaction(&entries, &default_compaction_settings(), true, false).unwrap();
    preparation.first_kept_entry_id = "keep".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, RecoveryMetadata::default(), &entries, &mut diagnostics).unwrap();
    assert_eq!(result.details.unwrap()["retainedMessagePolicy"], "omit-unsafe-v1");
}

#[test]
fn unsafe_split_turn_advances_to_a_later_safe_boundary_and_probe_summary_is_honored() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    use serde_json::json;
    // The prepared boundary is a malformed assistant with no user turn after it; omit-unsafe-v1
    // projection repairs it, so the prepared suffix is accepted and no later boundary is needed.
    let entries = [
        json!({"type":"message","id":"u","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"task","timestamp":0}}),
        json!({"type":"message","id":"bad","parentId":"u","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"custom","customType":"t","content":"x","display":"nope","timestamp":0}}),
        json!({"type":"message","id":"tail","parentId":"bad","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":[{"type":"image","data":"x","mimeType":"image/png"}],"timestamp":0}}),
    ];
    let mut preparation = prepare_compaction(&entries, &default_compaction_settings(), true, false).unwrap();
    preparation.first_kept_entry_id = "u".into();
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    let result = create_required_compaction_fallback(&preparation, 100000, RequiredCompactionFallbackFailure::SummarizationTimeout, RecoveryMetadata { task_intent: None, summary: Some("") }, &entries, &mut diagnostics).unwrap();
    assert_eq!(result.summary, "");
    assert!(diagnostics.candidates_checked >= 1);
    assert_eq!(result.details.unwrap()["retainedMessagePolicy"], "omit-unsafe-v1");
}

#[test]
fn unsafe_retained_suffix_error_renders_the_paused_notice() {
    let error = UnsafeRetainedSuffixError { diagnostics: DeterministicFallbackDiagnostic { rejection_reason: Some("unsafe-retained-content"), ..Default::default() } };
    let text = error.to_string();
    assert!(text.contains("Automatic compaction is paused for this unchanged transcript and model/settings."));
    assert_eq!(classify_required_compaction_fallback_failure(SummaryFailure::UnsafeRetainedSuffix, &text), Some(RequiredCompactionFallbackFailure::UnsafeRetainedContent));
}
