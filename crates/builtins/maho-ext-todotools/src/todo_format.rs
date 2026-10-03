use crate::todo_types::{TodoItem, TodoPhase, TodoStatus};
use std::sync::LazyLock;
use regex::Regex;

pub fn sanitize_todo_text(text: &str) -> String {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(
        r"(?:\x1b\][\s\S]*?(?:\x07|\x1b\\|\x{009c}))|[\x1b\x{009b}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]"
    ).unwrap_or_else(|error| panic!("invalid static ANSI expression: {error}")));
    let stripped = ANSI.replace_all(text, "");
    let cleaned: String = stripped.chars().map(|c| {
        if c <= '\u{001f}' || ('\u{007f}'..='\u{009f}').contains(&c) { ' ' } else { c }
    }).collect();
    cleaned.split(|c: char| c.is_whitespace() || c == '\u{feff}').filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn is_terminal_todo_status(status: &str) -> bool {
    matches!(status, "completed" | "abandoned" | "cancelled")
}
pub fn is_incomplete_todo(todo: &TodoItem) -> bool {
    matches!(todo.status, TodoStatus::Pending | TodoStatus::InProgress)
}
pub fn get_todo_marker(status: &str) -> &'static str {
    match status { "completed" => "[✓]", "in_progress" => "[•]", "abandoned" | "cancelled" => "[×]", _ => "[ ]" }
}
fn status_text(status: TodoStatus) -> &'static str {
    match status { TodoStatus::Pending => "pending", TodoStatus::InProgress => "in_progress", TodoStatus::Completed => "completed", TodoStatus::Abandoned => "abandoned" }
}
pub fn get_todo_result_lines(phases: &[TodoPhase]) -> Vec<String> {
    let mut lines = vec![format!("{} todos", phases.iter().flat_map(|p| &p.tasks).filter(|t| is_incomplete_todo(t)).count())];
    for phase in phases {
        lines.push(format!("{}:", sanitize_todo_text(&phase.name)));
        lines.extend(phase.tasks.iter().map(|t| format!("{} {}", get_todo_marker(status_text(t.status)), sanitize_todo_text(&t.content))));
    }
    lines
}
pub fn format_summary(phases: &[TodoPhase], errors: &[String], read_only: bool) -> String {
    let total = phases.iter().map(|p| p.tasks.len()).sum::<usize>();
    if total == 0 {
        return if !errors.is_empty() { format!("Errors: {}", errors.join("; ")) }
            else if read_only { "Todo list is empty.".into() } else { "Todo list cleared.".into() };
    }
    let remaining: Vec<_> = phases.iter().flat_map(|p| p.tasks.iter().filter(|t| is_incomplete_todo(t)).map(move |t| (p,t))).collect();
    let current_idx = phases.iter().position(|p| p.tasks.iter().any(is_incomplete_todo)).unwrap_or(phases.len()-1);
    let current = &phases[current_idx];
    let closed = |p: &TodoPhase| p.tasks.iter().filter(|t| !is_incomplete_todo(t)).count();
    let mut lines = vec![];
    if !errors.is_empty() { lines.push(format!("Errors: {}", errors.join("; "))); }
    if remaining.is_empty() { lines.push("Remaining items: none.".into()); }
    else {
        lines.push(format!("Remaining items ({}):", remaining.len()));
        lines.extend(remaining.iter().map(|(p,t)| format!("  - {} [{}] ({})", t.content,status_text(t.status),p.name)));
    }
    lines.push(format!("Overall: {}/{} done, {} open.", phases.iter().map(closed).sum::<usize>(),total,remaining.len()));
    let suffix = if phases.iter().skip(current_idx+1).any(|p| closed(p)>0) {
        " — earliest phase with open tasks; the in-progress pointer auto-advances to the earliest open task on each completion, so it can sit behind out-of-order work (nothing was un-completed)."
    } else { "." };
    lines.push(format!("Active phase {}/{} \"{}\" ({}/{}){}",current_idx+1,phases.len(),current.name,closed(current),current.tasks.len(),suffix));
    for p in phases {
        lines.push(format!("  {}:",p.name));
        for t in &p.tasks {
            let checkbox = if t.status == TodoStatus::Completed { "[X]" } else { "[ ]" };
            let tag = match t.status { TodoStatus::InProgress => " (in progress)", TodoStatus::Abandoned => " (dropped)", TodoStatus::Pending | TodoStatus::Completed => "" };
            lines.push(format!("    - {checkbox} {}{tag}",t.content));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn sanitizes_ansi_controls_and_whitespace() {
        assert_eq!(sanitize_todo_text("\x1b[31mDeploy\x1b[0m\r\n  API\x00\u{feff}"), "Deploy API");
    }
    #[test] fn strips_osc_hyperlink_but_retains_label() {
        assert_eq!(sanitize_todo_text("\x1b]8;;https://example.org\x07label\x1b]8;;\x1b\\"), "label");
    }
    #[test] fn result_counts_only_open_tasks() {
        let p = vec![TodoPhase { name:"Tasks".into(), tasks:vec![TodoItem {content:"Done".into(),status:TodoStatus::Completed},TodoItem {content:"Work".into(),status:TodoStatus::Pending}] }];
        assert_eq!(get_todo_result_lines(&p),vec!["1 todos","Tasks:","[✓] Done","[ ] Work"]);
    }
    #[test] fn empty_summary_respects_read_only_and_errors() {
        assert_eq!(format_summary(&[], &[], true), "Todo list is empty.");
        assert_eq!(format_summary(&[], &[], false), "Todo list cleared.");
        assert_eq!(format_summary(&[], &["missing".into()], false), "Errors: missing");
    }
}
