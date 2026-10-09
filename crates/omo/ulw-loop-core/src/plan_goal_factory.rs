//! Goal creation and candidate derivation from brief text.

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

fn clean_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix(|c| c == '-' || c == '*' || c == '+') {
        return rest.trim().to_string();
    }
    let mut chars = trimmed.chars().peekable();
    let mut num_end = 0;
    while let Some(c) = chars.peek() {
        if c.is_ascii_digit() {
            num_end += 1;
            chars.next();
        } else {
            break;
        }
    }
    if num_end > 0 {
        let after_digits = &trimmed[num_end..];
        if after_digits.starts_with('.') || after_digits.starts_with(')') {
            return after_digits[1..].trim().to_string();
        }
    }
    trimmed.trim().to_string()
}

fn normalize_objective(value: &str) -> String {
    let mut words = Vec::new();
    for word in value.split_whitespace() {
        words.push(word);
    }
    words.join(" ")
}

fn title_from_objective(objective: &str, fallback: &str) -> String {
    let first_line = objective
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or(fallback);
    if first_line.len() > 72 {
        let truncated = first_line[..69].trim_end();
        format!("{truncated}...")
    } else {
        first_line.to_string()
    }
}

pub fn normalize_goal_id(title: &str, index: usize) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for c in title.to_ascii_lowercase().chars() {
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
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UlwLoopError::new(format!("Missing {label}."), "ULW_LOOP_ARGUMENT_MISSING"));
    }
    Ok(trimmed.to_string())
}

fn truncate_objective(objective: &str) -> String {
    if objective.len() > 80 {
        format!("{}...", objective[..77].trim_end())
    } else {
        objective.to_string()
    }
}

fn replace_via(goal_id: &str, id: &str, surface: UlwLoopToolkitSurface) -> String {
    match surface {
        UlwLoopToolkitSurface::OmoSenpi => format!(
            "Replace via agentToolkit.steer({{ kind: \"revise_criterion\", source: \"finding\", goalId: \"{goal_id}\", criterionId: \"{id}\", scenario, expectedEvidence, evidence, rationale }}) (or pass successCriteria to addGoal)"
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
    let mut bullet_goals = Vec::new();
    for line in brief.lines() {
        let trimmed_start = line.trim_start();
        let is_bullet = trimmed_start.starts_with(|c| c == '-' || c == '*' || c == '+')
            || (trimmed_start.chars().next().is_some_and(|c| c.is_ascii_digit())
                && (trimmed_start.contains('.') || trimmed_start.contains(')')));
        if is_bullet {
            let cleaned = normalize_objective(&clean_line(line));
            if !cleaned.is_empty() && cleaned.len() <= 1200 && !bullet_goals.contains(&cleaned) {
                bullet_goals.push(cleaned);
            }
        }
    }

    let mut paragraphs = Vec::new();
    for p in brief.split("\n\n") {
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
