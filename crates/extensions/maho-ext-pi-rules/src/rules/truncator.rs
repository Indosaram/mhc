//! UTF-16 character budgets, matching JavaScript string lengths.

#[derive(Debug, PartialEq, Eq)]
pub struct TruncationResult {
    pub body: String,
    pub truncated: bool,
    pub original_length: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BudgetRule {
    pub body: String,
    pub relative_path: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BudgetResult {
    pub body: String,
    pub relative_path: String,
    pub truncated: bool,
}

fn notice(path: &str) -> String {
    format!("\n\n[Rule truncated. Read full rule: {path}]")
}

fn length(body: &str) -> usize {
    body.encode_utf16().count()
}

fn prefix(body: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, character) in body.char_indices() {
        units += character.len_utf16();
        if units > max_units {
            return &body[..index];
        }
    }
    body
}

pub fn truncate_rule(body: &str, max_chars: usize, relative_path: &str) -> TruncationResult {
    let original_length = length(body);
    if original_length <= max_chars {
        return TruncationResult { body: body.into(), truncated: false, original_length };
    }
    let suffix = notice(relative_path);
    let available = max_chars.saturating_sub(length(&suffix));
    TruncationResult {
        body: format!("{}{suffix}", prefix(body, available)),
        truncated: true,
        original_length,
    }
}

pub fn truncate_budget(rules: &[BudgetRule], max_result_chars: usize) -> Vec<BudgetResult> {
    let mut results = Vec::new();
    let mut remaining = max_result_chars;
    for rule in rules {
        let body_length = length(&rule.body);
        if remaining >= body_length {
            results.push(BudgetResult { body: rule.body.clone(), relative_path: rule.relative_path.clone(), truncated: false });
            remaining -= body_length;
            continue;
        }
        let suffix = notice(&rule.relative_path);
        let suffix_length = length(&suffix);
        if remaining <= suffix_length {
            break;
        }
        let body = format!("{}{suffix}", prefix(&rule.body, remaining - suffix_length));
        remaining -= length(&body);
        results.push(BudgetResult { body, relative_path: rule.relative_path.clone(), truncated: true });
    }
    results
}
