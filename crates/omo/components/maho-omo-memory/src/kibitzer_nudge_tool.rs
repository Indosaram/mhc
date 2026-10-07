//! Kibitzer nudge tool base (latest `kibitzer/nudge-tool.ts`).
//!
//! The kibitzer judge's ONLY output channel, as a stateless closure over ONE launch's input: a
//! candidate path plus a single-sentence factual hint, validated synchronously at call time so a
//! rejected call returns an error result the judge can read and correct. Hint rules come from
//! memory-core's `describe_invalid_hint`; candidate, surfaced and `system/` membership mirror the
//! parent's `validate_nudges`, which the parent still runs over the collected set before persisting
//! (defence in depth - duplicates, should the judge repeat a path, are dropped there, not here).
//!
//! This module is the BASE half. It owns the launch input (`candidates`, `surfaced`, `max_items`,
//! `accepted`) plus the exact rejection/acceptance texts and the `terminate` flag, and holds NO
//! per-wake or global state: the wrapper (`kibitzer/tools/nudge.ts`) resolves the live allowed set
//! (`offered` union `searched`), the current wake's accepted list and the wake budget at call time,
//! and re-binds this base per call, charging the budget exactly once (a rejected call consumes one
//! charge, never two).
//!
//! Once `accepted` reaches `max_items` nothing more can be recorded this run, so the result carries
//! `terminate: true` and the agent loop ends the turn on the tool batch. Without it the judge would
//! have to produce one more assistant message with nothing to say, and senpi's empty-assistant
//! recovery settles a second silent stop as an error (#7963).

use std::collections::BTreeSet;

use memory_core::recall::gate::{InvalidHintReason, RecallNudge, describe_invalid_hint};
use memory_core::sync::redact::contains_secret_like_material;
use serde::{Deserialize, Serialize};

use crate::kibitzer_tools_result::KibitzerToolResult;

/// The registered tool name (`nudge`).
pub const KIBITZER_NUDGE_TOOL_NAME: &str = "nudge";

/// The upstream tool label (`Kibitzer`); the native `ToolDefinition` carries no label field.
pub const KIBITZER_NUDGE_TOOL_LABEL: &str = "Kibitzer";

/// The tool description, verbatim from upstream.
pub const KIBITZER_NUDGE_DESCRIPTION: &str =
    "Surface one stored memory to the primary agent as a read-only hint. Call it only when the memory would change what the agent does next.";

/// The `nudge` parameters (`Static<typeof KibitzerNudgeParams>`): a memory path copied from the
/// candidates plus the single-sentence hint about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KibitzerNudgeParams {
    pub path: String,
    pub hint: String,
}

/// The `nudge` parameter schema, mirroring the upstream TypeBox object (`additionalProperties:
/// false`, both properties required).
pub fn kibitzer_nudge_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "hint"],
        "properties": {
            "path": {
                "type": "string",
                "description": "Memory path copied exactly from the candidates input."
            },
            "hint": {
                "type": "string",
                "description": "One factual sentence stating what the stored note records ('the note records that ...'), at most 200 characters, on a single line. It is reference the agent reads, so never address it or tell it what to do: no second person, no imperative, no judgment about whether to nudge, and never filler such as 'placeholder'."
            }
        }
    })
}

/// The launch input the base validates against. Every field is borrowed: the wrapper resolves the
/// live allowed set, the session-scoped surfaced set and the CURRENT wake's accepted list before
/// every call, so nothing here survives the call.
pub struct KibitzerNudgeInput<'a> {
    /// Paths offered this launch (or returned by the corpus search); anything else is fabricated.
    pub candidates: &'a BTreeSet<String>,
    /// Paths already surfaced in this session; they never repeat.
    pub surfaced: &'a BTreeSet<String>,
    /// Authoritative cap (`memory.recall.max_items`) for THIS run's accepted nudges.
    pub max_items: usize,
    /// The array the runner owns: every accepted nudge is recorded here, in call order.
    pub accepted: &'a mut Vec<RecallNudge>,
}

/// The base `nudge` execute: reject against the launch input in source order, else record the nudge
/// in call order. A rejection returns the error text with the current cap termination; an acceptance
/// returns the recorded text and terminates once the cap is reached.
pub fn execute_nudge(
    input: KibitzerNudgeInput<'_>,
    params: &KibitzerNudgeParams,
) -> KibitzerToolResult {
    if let Some(rejection) = reject_nudge(params, &input) {
        return KibitzerToolResult {
            text: format!("Nudge rejected: {rejection} Correct the call once, or end the run."),
            is_error: true,
            terminate: cap_reached(&input),
        };
    }
    input.accepted.push(RecallNudge { path: params.path.clone(), hint: params.hint.clone() });
    let capped = cap_reached(&input);
    let text = if capped {
        format!(
            "Nudge recorded for {}. The maxItems limit ({}) is reached; the run ends here.",
            params.path, input.max_items
        )
    } else {
        format!("Nudge recorded for {}.", params.path)
    };
    KibitzerToolResult { text, is_error: false, terminate: capped }
}

/// Once `accepted` reaches `max_items` nothing more can be recorded this run.
fn cap_reached(input: &KibitzerNudgeInput<'_>) -> bool {
    input.accepted.len() >= input.max_items
}

/// The first rule the call breaks, in source order: candidate, surfaced, `system/`, secret, invalid
/// hint, accepted cap. `None` means the call is admitted. Duplicates are NOT rejected here - the
/// parent's `validate_nudges` deduplicates the collected set later.
fn reject_nudge(params: &KibitzerNudgeParams, input: &KibitzerNudgeInput<'_>) -> Option<String> {
    if !input.candidates.contains(&params.path) {
        return Some(format!("\"{}\" is not one of the offered candidates.", params.path));
    }
    if input.surfaced.contains(&params.path) {
        return Some(format!("\"{}\" was already surfaced in this session.", params.path));
    }
    if params.path == "system/" || params.path.starts_with("system/") {
        return Some(format!("\"{}\" is a system/ path and cannot be nudged.", params.path));
    }
    // Secret-bearing material is named first: it is the more urgent correction when a hint breaks
    // both rules at once.
    if contains_secret_like_material(&params.hint) {
        return Some("The hint was rejected because it contains secret-like material.".to_string());
    }
    match describe_invalid_hint(&params.hint) {
        Some(InvalidHintReason::AddressesAgent) => Some(
            "The hint addresses the agent (second person or imperative): restate what the note records as a plain observation."
                .to_string(),
        ),
        Some(_) => Some(
            "The hint must state a memory fact in one non-empty line of at most 200 characters, not comment on whether the memory is relevant or worth nudging."
                .to_string(),
        ),
        None if input.accepted.len() >= input.max_items => {
            Some(format!("The maxItems limit ({}) for this run has been reached.", input.max_items))
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::recall::gate::NUDGE_HINT_MAX_CHARS;

    const CANDIDATE_PATH: &str = "reference/kubernetes-rollouts.md";
    const SURFACED_PATH: &str = "notes/quiet.md";
    const SYSTEM_PATH: &str = "system/persona.md";
    /// The nudge-only shape the contract asks for: what the stored note records, not what to do.
    /// Used in place of the upstream historical `HINT` fixture, whose imperative opening is admitted
    /// only through the fixed opening-verb list (see the receipt's finding).
    const OBSERVATION: &str = "The rollout note records that nodes are drained before a rollout.";

    fn candidates() -> BTreeSet<String> {
        [CANDIDATE_PATH, SURFACED_PATH, SYSTEM_PATH]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn surfaced_set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_string()).collect()
    }

    fn recorded(path: &str, hint: &str) -> RecallNudge {
        RecallNudge { path: path.to_string(), hint: hint.to_string() }
    }

    fn call(
        candidates: &BTreeSet<String>,
        surfaced: &BTreeSet<String>,
        max_items: usize,
        accepted: &mut Vec<RecallNudge>,
        path: &str,
        hint: &str,
    ) -> KibitzerToolResult {
        let params = KibitzerNudgeParams { path: path.to_string(), hint: hint.to_string() };
        execute_nudge(KibitzerNudgeInput { candidates, surfaced, max_items, accepted }, &params)
    }

    #[test]
    fn given_a_valid_candidate_and_hint_when_nudged_then_it_is_recorded_unchanged_and_reported() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, OBSERVATION);

        assert!(!result.is_error);
        assert!(!result.terminate);
        assert!(result.text.contains(CANDIDATE_PATH));
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_a_path_outside_the_candidate_set_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result =
            call(&candidates, &surfaced, 2, &mut accepted, "notes/never-offered.md", OBSERVATION);

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_path_already_surfaced_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = surfaced_set(&[SURFACED_PATH]);
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 2, &mut accepted, SURFACED_PATH, OBSERVATION);

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_system_path_among_the_candidates_when_nudged_then_it_is_rejected_and_nothing_is_recorded()
    {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 2, &mut accepted, SYSTEM_PATH, OBSERVATION);

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_blank_hint_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, "");

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_hint_over_the_character_budget_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let over = "x".repeat(NUDGE_HINT_MAX_CHARS + 1);

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, &over);

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_secret_bearing_hint_when_nudged_then_the_secret_rule_precedes_the_invalid_hint_rule() {
        // The hint is both secret-bearing and imperative-opening; the secret reason must win.
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(
            &candidates,
            &surfaced,
            2,
            &mut accepted,
            CANDIDATE_PATH,
            "Use AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE.",
        );

        assert!(result.is_error);
        assert!(accepted.is_empty());
        assert!(result.text.contains("secret-like material"));
    }

    #[test]
    fn given_a_password_assignment_hint_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result =
            call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, "password=hunter2");

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_bearer_authorization_hint_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(
            &candidates,
            &surfaced,
            2,
            &mut accepted,
            CANDIDATE_PATH,
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.abc",
        );

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_key_shaped_hint_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result =
            call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, "sk-proj-AAAABBBBCCCCDDDD");

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_multiline_hint_when_nudged_then_it_is_rejected_and_nothing_is_recorded() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result =
            call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, "line one\nline two");

        assert!(result.is_error);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_the_first_meta_hint_when_nudged_then_it_is_rejected_and_a_factual_correction_still_uses_the_cap()
    {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let meta = "No stored memory clears the bar for this planning step; the transcript already contains the full methodology, QA approach, and rollout.";

        let rejected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, meta);
        assert!(rejected.is_error);
        assert!(accepted.is_empty());

        let corrected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!corrected.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_second_meta_hint_when_nudged_then_it_is_rejected_and_a_factual_correction_still_uses_the_cap()
    {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let meta = "This memory covers OAuth login prompts and remote-test helpers, not the goal continuation timer delay.";

        let rejected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, meta);
        assert!(rejected.is_error);
        assert!(accepted.is_empty());

        let corrected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!corrected.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_first_agent_addressing_hint_when_nudged_then_it_is_rejected_and_one_correction_lands() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let rejected =
            call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, "Verify these before continuing.");
        assert!(rejected.is_error);
        assert!(accepted.is_empty());

        let corrected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!corrected.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_second_agent_addressing_hint_when_nudged_then_it_is_rejected_and_one_correction_lands() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let rejected = call(
            &candidates,
            &surfaced,
            1,
            &mut accepted,
            CANDIDATE_PATH,
            "Do not rebase the worktree while a child task writes in it.",
        );
        assert!(rejected.is_error);
        assert!(accepted.is_empty());

        let corrected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!corrected.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_third_agent_addressing_hint_when_nudged_then_it_is_rejected_and_one_correction_lands() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let rejected = call(
            &candidates,
            &surfaced,
            1,
            &mut accepted,
            CANDIDATE_PATH,
            "You must keep the guard green before publishing.",
        );
        assert!(rejected.is_error);
        assert!(accepted.is_empty());

        let corrected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!corrected.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_korean_agent_addressing_hint_when_nudged_then_it_is_rejected_and_one_correction_lands() {
        // Upstream fixture: "머지 전에 그린 메인 가드를 확인하십시오." (Korean request ending).
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let rejected = call(
            &candidates,
            &surfaced,
            1,
            &mut accepted,
            CANDIDATE_PATH,
            "\u{ba38}\u{c9c0} \u{c804}\u{c5d0} \u{adf8}\u{b9b0} \u{ba54}\u{c778} \u{ac00}\u{b4dc}\u{b97c} \u{d655}\u{c778}\u{d558}\u{c2ed}\u{c2dc}\u{c624}.",
        );
        assert!(rejected.is_error);
        assert!(accepted.is_empty());

        let corrected = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!corrected.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_first_factual_hint_when_nudged_then_it_is_accepted_unchanged() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let hint = "The fix is on senpi main, not the extension.";

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, hint);

        assert!(!result.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, hint)]);
    }

    #[test]
    fn given_the_second_factual_hint_when_nudged_then_it_is_accepted_unchanged() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let hint = "senpi monitors have a verified two-flag desync where registry.paused can remain set.";

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, hint);

        assert!(!result.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, hint)]);
    }

    #[test]
    fn given_the_third_factual_hint_when_nudged_then_it_is_accepted_unchanged() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let hint = "The regression test does not cover Windows process cleanup.";

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, hint);

        assert!(!result.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, hint)]);
    }

    #[test]
    fn given_the_fourth_factual_hint_when_nudged_then_it_is_accepted_unchanged() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let hint = "The outage is unrelated to the database migration.";

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, hint);

        assert!(!result.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, hint)]);
    }

    #[test]
    fn given_the_budget_already_spent_when_nudged_again_then_it_is_rejected_and_the_accepted_set_is_unchanged()
    {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let first = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!first.is_error);

        let second = call(&candidates, &surfaced, 1, &mut accepted, SURFACED_PATH, OBSERVATION);

        assert!(second.is_error);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_max_items_two_when_the_first_nudge_is_accepted_then_the_loop_stays_open() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, OBSERVATION);

        assert!(!result.is_error);
        assert!(!result.terminate);
    }

    #[test]
    fn given_max_items_one_when_the_accepting_nudge_reaches_the_cap_then_the_result_terminates() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);

        assert!(!result.is_error);
        assert!(result.terminate);
        assert_eq!(accepted, vec![recorded(CANDIDATE_PATH, OBSERVATION)]);
    }

    #[test]
    fn given_the_cap_already_reached_when_a_further_nudge_is_rejected_then_the_rejection_also_terminates()
    {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();
        let _ = call(&candidates, &surfaced, 1, &mut accepted, CANDIDATE_PATH, OBSERVATION);

        let result = call(&candidates, &surfaced, 1, &mut accepted, SURFACED_PATH, OBSERVATION);

        assert!(result.is_error);
        assert!(result.terminate);
    }

    #[test]
    fn given_a_rejected_nudge_below_the_cap_when_inspected_then_the_loop_stays_open() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result =
            call(&candidates, &surfaced, 2, &mut accepted, "notes/never-offered.md", OBSERVATION);

        assert!(result.is_error);
        assert!(!result.terminate);
    }

    #[test]
    fn given_the_tool_definition_when_inspected_then_the_name_and_required_parameters_are_machine_consumed()
    {
        assert_eq!(KIBITZER_NUDGE_TOOL_NAME, "nudge");

        let schema = kibitzer_nudge_parameters();
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        let required: Vec<String> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();
        assert_eq!(required, vec!["path".to_string(), "hint".to_string()]);
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["hint"]["type"], "string");
    }

    #[test]
    fn given_nudge_params_when_serialized_then_only_path_and_hint_are_emitted() {
        let params =
            KibitzerNudgeParams { path: CANDIDATE_PATH.to_string(), hint: OBSERVATION.to_string() };

        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value, serde_json::json!({ "path": CANDIDATE_PATH, "hint": OBSERVATION }));

        let round_tripped: KibitzerNudgeParams = serde_json::from_value(value).unwrap();
        assert_eq!(round_tripped, params);
    }

    #[test]
    fn given_a_zero_cap_when_nudged_then_nothing_is_recorded_and_the_rejection_terminates() {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let result = call(&candidates, &surfaced, 0, &mut accepted, CANDIDATE_PATH, OBSERVATION);

        assert!(result.is_error);
        assert!(result.terminate);
        assert!(accepted.is_empty());
    }

    #[test]
    fn given_a_duplicate_path_below_the_cap_when_nudged_then_the_base_records_both_without_a_duplicate_policy()
    {
        let candidates = candidates();
        let surfaced = BTreeSet::new();
        let mut accepted = Vec::new();

        let first = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!first.is_error);

        let second = call(&candidates, &surfaced, 2, &mut accepted, CANDIDATE_PATH, OBSERVATION);
        assert!(!second.is_error);
        assert!(second.terminate);
        assert_eq!(
            accepted,
            vec![recorded(CANDIDATE_PATH, OBSERVATION), recorded(CANDIDATE_PATH, OBSERVATION)]
        );
    }
}
