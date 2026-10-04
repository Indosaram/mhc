//! Objective truncation and inert token-budget validation.
use crate::errors::GoalError;
pub const MAX_OBJECTIVE_LENGTH: usize = 4_000;
const WHITESPACE_LOOKBACK: usize = 200;
pub(crate) fn js_whitespace(c: char) -> bool { matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}') }
pub fn is_goal_status(value: &serde_json::Value) -> bool { matches!(value.as_str(), Some("active" | "paused" | "blocked" | "complete")) }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedObjective { pub objective: String, pub truncated: bool, pub full_text_file_name: Option<String> }
pub fn truncation_marker(file: &str) -> String { format!("… [truncated; full objective: {file}]") }
pub fn objective_truncation_notice(file: &str) -> String { format!("Objective was truncated; full objective saved to {file}.") }
pub fn validate_objective(value: &str, file: &str) -> Result<ValidatedObjective, GoalError> {
    let objective = value.trim_matches(js_whitespace);
    if objective.is_empty() { return Err(GoalError::InvalidMutation("objective must not be empty".into())); }
    let points: Vec<char> = objective.chars().collect();
    if points.len() <= MAX_OBJECTIVE_LENGTH {
        return Ok(ValidatedObjective { objective: objective.into(), truncated: false, full_text_file_name: None });
    }
    let marker = truncation_marker(file);
    let budget = MAX_OBJECTIVE_LENGTH.saturating_sub(marker.chars().count());
    let cut = (budget.saturating_sub(WHITESPACE_LOOKBACK)..budget).rev().find(|&i| points.get(i).is_some_and(|c| js_whitespace(*c))).unwrap_or(budget);
    let mut payload: String = points[..cut].iter().collect();
    payload.push_str(&marker);
    Ok(ValidatedObjective { objective: payload, truncated: true, full_text_file_name: Some(file.into()) })
}
pub fn is_non_negative_safe_integer(value: f64) -> bool { value.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&value) && value.fract()==0.0 }
pub fn validate_token_budget(value: u64) -> Result<u64, GoalError> {
    if value > 9_007_199_254_740_991 { return Err(GoalError::InvalidMutation("token budget must be a non-negative integer".into())); }
    Ok(value)
}
pub fn resolve_token_budget(current: Option<u64>, update: Option<Option<f64>>) -> Result<Option<u64>, GoalError> {
    match update { None => Ok(current), Some(None) => Ok(None), Some(Some(value)) if is_non_negative_safe_integer(value)=>Ok(Some(value as u64)),Some(Some(_))=>Err(GoalError::InvalidMutation("token budget must be a non-negative integer".into())) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn safe_integer_rejects_all_fractional_values_including_subnormal_numbers() {
        for value in [1e-17,f64::from_bits(1),0.5,9_007_199_254_740_992.0,f64::NAN,f64::INFINITY,-1.0] { assert!(!is_non_negative_safe_integer(value)); }
        for value in [-0.0,0.0,1.0,9_007_199_254_740_991.0] { assert!(is_non_negative_safe_integer(value)); }
    }
    #[test] fn empty_objective_is_rejected() { let value = "  \n"; let result = validate_objective(value, "full.txt"); assert!(result.is_err()); }
    #[test] fn objective_is_trimmed_without_truncation() { let value = "  build it  "; let result = validate_objective(value, "full.txt").unwrap(); assert_eq!(result.objective, "build it"); assert!(!result.truncated); }
    #[test] fn unicode_objective_uses_code_points() { let value = "😀".repeat(4001); let result = validate_objective(&value, "full.txt").unwrap(); assert_eq!(result.objective.chars().count(), 4000); assert!(result.truncated); }
    #[test] fn whitespace_cut_stays_within_lookback() { let value = format!("{} {}", "a".repeat(3800), "b".repeat(300)); let result = validate_objective(&value, "full.txt").unwrap(); assert_eq!(result.objective, format!("{}{}", "a".repeat(3800), truncation_marker("full.txt"))); }
    #[test] fn budget_update_preserves_clears_and_rejects_unsafe_values() { let current = Some(100); let result = (resolve_token_budget(current, None), resolve_token_budget(current, Some(None)), resolve_token_budget(current, Some(Some(u64::MAX as f64)))); assert_eq!(result.0.unwrap(), Some(100)); assert_eq!(result.1.unwrap(), None); assert!(result.2.is_err()); }
    #[test] fn trim_uses_javascript_whitespace() { let value = "\u{feff}work\u{0085}"; let result = validate_objective(value, "full.txt").unwrap(); assert_eq!(result.objective, "work\u{0085}"); }
}
