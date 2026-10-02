use super::{truncator::{truncate_budget, truncate_rule, BudgetRule}, types::LoadedRule};

pub struct FormatOptions { pub max_rule_chars: usize, pub max_result_chars: usize }
fn block(rules: &[LoadedRule], options: &FormatOptions, header: &str) -> String {
    let mut remaining = options.max_result_chars.saturating_sub(header.encode_utf16().count());
    let mut parts = Vec::new();
    for rule in rules {
        let body = truncate_rule(&rule.body, options.max_rule_chars, &rule.candidate.relative_path).body;
        let separator = if parts.is_empty() { "" } else { "\n\n" };
        let rule_header = format!("{separator}Instructions from: {}\n",rule.candidate.path);
        let overhead = rule_header.encode_utf16().count();
        if remaining < overhead { break; }
        let budgeted = truncate_budget(&[BudgetRule { body, relative_path: rule.candidate.relative_path.clone() }], remaining - overhead);
        let Some(budgeted) = budgeted.first() else { break; };
        remaining -= overhead + budgeted.body.encode_utf16().count();
        parts.push(format!("{rule_header}{}",budgeted.body));
    }
    if parts.is_empty() { String::new() } else { format!("{header}{}",parts.concat()) }
}
pub fn format_static_block(rules: &[LoadedRule], options: &FormatOptions) -> String {
    block(rules,options,"\n\n## Project Instructions\n")
}
pub fn format_dynamic_block(rules: &[LoadedRule], target_relative_path: &str, options: &FormatOptions) -> String {
    block(rules,options,&format!("\n\nAdditional project instructions matched for {target_relative_path}:\n\n"))
}
