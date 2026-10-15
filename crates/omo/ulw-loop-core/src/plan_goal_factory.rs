//! Goal creation and candidate derivation from brief text.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::runtime::UlwLoopError;
use crate::success_criteria_input::{criteria_from_input, criterion_id};
use crate::surface::UlwLoopToolkitSurface;

#[derive(Clone, Debug, Default)]
pub struct GoalSeedOptions {
    pub goal_id: Option<String>,
    pub surface: Option<UlwLoopToolkitSurface>,
    pub success_criteria: Option<Vec<Value>>,
}

/// JS `\s` / `String.prototype.trim` whitespace: unlike Rust's White_Space it includes U+FEFF
/// (BOM) and excludes U+0085 (NEL).
const JS_WS: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

fn is_js_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}

/// JS `value.trim()`.
fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// `/^\s*(?:[-*+]\s+|\d+[.)]\s+)/`: a list marker must be followed by whitespace (JS `\d` is ASCII).
fn list_marker_pattern() -> String {
    format!(r"^{JS_WS}*(?:[-*+]{JS_WS}+|[0-9]+[.)]{JS_WS}+)")
}

static LIST_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&list_marker_pattern()).expect("valid list marker regex"));

static PARAGRAPH_BREAK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\n{JS_WS}*\n")).expect("valid paragraph regex")
});

fn list_marker() -> &'static Regex {
    &LIST_MARKER
}

/// JS `string.length`: UTF-16 code units.
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// JS `string.slice(0, end)` on UTF-16 units, cut on a char boundary (a surrogate pair straddling
/// `end` is kept whole, as `maho_tui::components::markdown_lexer::slice_utf16` does).
fn slice_utf16_prefix(value: &str, end: usize) -> &str {
    let mut units = 0usize;
    for (byte, character) in value.char_indices() {
        if units >= end {
            return &value[..byte];
        }
        units += character.len_utf16();
    }
    value
}

fn clean_line(line: &str) -> String {
    js_trim(&list_marker().replace(line, "")).to_string()
}

fn normalize_objective(value: &str) -> String {
    let mut words = Vec::new();
    for word in value.split(is_js_whitespace).filter(|word| !word.is_empty()) {
        words.push(word);
    }
    words.join(" ")
}

fn title_from_objective(objective: &str, fallback: &str) -> String {
    let first_line = objective
        .lines()
        .map(js_trim)
        .find(|l| !l.is_empty())
        .unwrap_or(fallback);
    if utf16_len(first_line) > 72 {
        let truncated = slice_utf16_prefix(first_line, 69).trim_end_matches(is_js_whitespace);
        format!("{truncated}...")
    } else {
        first_line.to_string()
    }
}

pub fn normalize_goal_id(title: &str, index: usize) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    // JS `toLowerCase` is full Unicode: U+0130 and U+212A lowercase into ASCII before the filter.
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let trimmed_slug = slug.trim_matches('-');
    let clipped = if trimmed_slug.len() > 36 {
        trimmed_slug[..36].trim_end_matches('-')
    } else {
        trimmed_slug
    };

    if clipped.is_empty() {
        format!("G{:03}", index + 1)
    } else {
        format!("G{:03}-{clipped}", index + 1)
    }
}

fn assert_non_empty(value: &str, label: &str) -> Result<String, UlwLoopError> {
    let trimmed = js_trim(value);
    if trimmed.is_empty() {
        return Err(UlwLoopError::new(format!("Missing {label}."), "ULW_LOOP_ARGUMENT_MISSING"));
    }
    Ok(trimmed.to_string())
}

fn truncate_objective(objective: &str) -> String {
    if utf16_len(objective) > 80 {
        format!("{}...", slice_utf16_prefix(objective, 77).trim_end_matches(is_js_whitespace))
    } else {
        objective.to_string()
    }
}

fn replace_via(goal_id: &str, id: &str, surface: UlwLoopToolkitSurface) -> String {
    match surface {
        UlwLoopToolkitSurface::OmoSenpi => format!(
            "Replace via mhc ulw-loop steer --kind revise_criterion --source finding --goal-id {goal_id} --criterion-id {id} --scenario \"<scenario>\" --expected-evidence \"<proof>\" --evidence \"<why>\" --rationale \"<why>\" (successCriteria cannot be passed to add-goal)"
        ),
        UlwLoopToolkitSurface::LazyCodex => format!(
            "Replace via ulw-loop steer --kind revise_criterion --goal-id {goal_id} --criterion-id {id} --scenario \"<scenario>\" --expected-evidence \"<proof>\" --evidence \"<why>\" --rationale \"<why>\""
        ),
    }
}

pub fn seed_default_success_criteria(
    goal_index: usize,
    objective: &str,
    options: Option<&GoalSeedOptions>,
) -> Vec<UlwLoopSuccessCriterion> {
    let norm = normalize_objective(objective);
    let subject = truncate_objective(if norm.is_empty() {
        format!("Goal {}", goal_index + 1)
    } else {
        norm
    }.as_str());

    let goal_id = options
        .and_then(|o| o.goal_id.clone())
        .unwrap_or_else(|| format!("G{:03}", goal_index + 1));
    let surface = options
        .and_then(|o| o.surface)
        .unwrap_or(UlwLoopToolkitSurface::LazyCodex);

    let rows: [(&str, String, String, bool); 3] = [
        ("happy", format!("happy path for: {subject}"), format!("observable happy-path proof for goal {}", goal_index + 1), true),
        ("edge", "edge case (boundary/empty/malformed)".to_string(), format!("boundary or malformed-input proof for: {subject}"), true),
        ("regression", "regression: adjacent surface still works".to_string(), format!("regression proof for neighboring behavior after: {subject}"), false),
    ];

    rows.into_iter().enumerate().map(|(index, (user_model, scenario, proof, essential))| {
        let id = criterion_id(index);
        UlwLoopSuccessCriterion {
            id: id.clone(),
            scenario,
            user_model: user_model.to_string(),
            expected_evidence: format!("{} with {proof}.", replace_via(&goal_id, &id, surface)),
            essential: Some(essential),
            captured_evidence: None,
            status: "pending".to_string(),
            captured_at: None,
            notes: None,
            artifacts: None,
        }
    }).collect()
}

pub fn derive_goal_candidates(brief: &str) -> Vec<(String, String)> {
    let marker = list_marker();
    let lines: Vec<(&str, String)> = brief
        .lines()
        .map(|line| (line, normalize_objective(&clean_line(line))))
        .filter(|(_, cleaned)| !cleaned.is_empty() && utf16_len(cleaned) <= 1200)
        .collect();
    let bullet_goals: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(index, (original, cleaned))| {
            marker.is_match(original)
                && lines.iter().position(|(_, candidate)| candidate == cleaned) == Some(*index)
        })
        .map(|(_, (_, cleaned))| cleaned.clone())
        .collect();

    let mut paragraphs = Vec::new();
    for p in PARAGRAPH_BREAK.split(brief) {
        let cleaned = normalize_objective(p);
        if !cleaned.is_empty() && !cleaned.starts_with('#') {
            paragraphs.push(cleaned);
        }
    }

    let selected = if !bullet_goals.is_empty() {
        bullet_goals
    } else if !paragraphs.is_empty() {
        paragraphs
    } else {
        vec!["Complete the requested project objective.".to_string()]
    };

    selected
        .into_iter()
        .enumerate()
        .map(|(index, objective)| {
            let title = title_from_objective(&objective, &format!("Goal {}", index + 1));
            (title, objective)
        })
        .collect()
}

pub fn make_goal(
    title: &str,
    objective: &str,
    index: usize,
    now: &str,
    options: Option<&GoalSeedOptions>,
) -> Result<UlwLoopItem, UlwLoopError> {
    let clean_title = assert_non_empty(title, "title")?;
    let clean_objective = assert_non_empty(objective, "objective")?;
    let id = normalize_goal_id(&clean_title, index);

    let success_criteria = match options.and_then(|o| o.success_criteria.as_deref()) {
        None => {
            let mut seed_opts = options.cloned().unwrap_or_default();
            seed_opts.goal_id = Some(id.clone());
            seed_default_success_criteria(index, &clean_objective, Some(&seed_opts))
        }
        Some(input) => criteria_from_input(input)?,
    };

    Ok(UlwLoopItem {
        id,
        title: clean_title,
        objective: clean_objective,
        status: "pending".to_string(),
        success_criteria,
        attempt: 0,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        started_at: None,
        completed_at: None,
        failed_at: None,
        review_blocked_at: None,
        evidence: None,
        failure_reason: None,
        steering_status: None,
        superseded_by: None,
        supersedes: None,
        blocked_reason: None,
        blocker_signature: None,
        blocker_occurrence_count: None,
        required_external_decision: None,
        non_retriable: None,
        steering_evidence: None,
        steering_rationale: None,
    })
}

pub fn append_goal_to_plan(
    plan: &mut UlwLoopPlan,
    title: &str,
    objective: &str,
    now: &str,
    options: Option<&GoalSeedOptions>,
) -> Result<UlwLoopItem, UlwLoopError> {
    let goal = make_goal(title, objective, plan.goals.len(), now, options)?;
    plan.goals.push(goal.clone());
    plan.updated_at = now.to_string();
    Ok(goal)
}
