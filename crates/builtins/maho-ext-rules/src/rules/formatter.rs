use super::{constants::*, truncator::{BudgetRule, truncate_budget, truncate_rule}, types::LoadedRule};

pub struct FormatOptions { pub max_rule_chars: usize, pub max_result_chars: usize }

fn format_within_budget(rules: &[LoadedRule], options: &FormatOptions, render: impl Fn(&str) -> String) -> String {
    let mut budget = options.max_result_chars;
    let truncated: Vec<_> = rules.iter().map(|rule| BudgetRule {
        body: truncate_rule(&rule.body, options.max_rule_chars, &rule.candidate.relative_path).body,
        relative_path: rule.candidate.relative_path.clone(),
    }).collect();
    while budget > 0 {
        let budgeted = truncate_budget(&truncated, budget);
        if budgeted.is_empty() { return String::new(); }
        let body = budgeted.iter().zip(rules).map(|(entry, rule)| format!("Instructions from: {}\n{}", rule.candidate.path, entry.body)).collect::<Vec<_>>().join("\n\n");
        let block = render(&body);
        let length = block.encode_utf16().count();
        if length <= options.max_result_chars { return block; }
        budget = budget.saturating_sub(length.saturating_sub(options.max_result_chars));
    }
    String::new()
}

pub fn format_static_block(rules: &[LoadedRule], options: &FormatOptions) -> String {
    format_within_budget(rules, options, |body| {
        let body = format!("{PROJECT_RULES_HEADING}\n{body}")
            .replace(PROJECT_RULES_REGION_START_MARKER, "&lt;!--senpi:project-rules:1:start--&gt;")
            .replace(PROJECT_RULES_REGION_END_MARKER, "&lt;!--senpi:project-rules:1:end--&gt;")
            .replace(PROJECT_RULES_START_MARKER, "&lt;project_rules&gt;")
            .replace(PROJECT_RULES_END_MARKER, "&lt;/project_rules&gt;");
        format!("\n\n{PROJECT_RULES_REGION_START_MARKER}\n{PROJECT_RULES_START_MARKER}\n{body}\n{PROJECT_RULES_END_MARKER}\n{PROJECT_RULES_REGION_END_MARKER}")
    })
}

pub fn format_dynamic_block(rules: &[LoadedRule], target_relative_path: &str, options: &FormatOptions) -> String {
    format_within_budget(rules, options, |body| format!("\n\nAdditional project instructions matched for {target_relative_path}:\n\n{body}"))
}
