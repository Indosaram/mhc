//! Prompts for writing one context file, and the cleanup of a model's answer (spec §4).

use crate::plan::{Action, Plan, Target};

/// Characters of a child summary handed to its parent.
pub const SUMMARY_LIMIT: usize = 600;

fn bullet(label: &str, items: &[String]) -> Option<String> {
    (!items.is_empty()).then(|| format!("- {label}: {}", items.join(", ")))
}

/// The facts block of one target.
pub fn facts(target: &Target) -> String {
    let mut lines = vec![
        format!(
            "- files here: {}; source files here and below: {}",
            target.files, target.subtree_code_files
        ),
    ];
    lines.extend(bullet("main languages", &target.languages));
    lines.extend(bullet("build/package files here", &target.manifests));
    lines.extend(bullet("likely entry points here", &target.entry_points));
    if target.has_tests {
        lines.push("- tests: present".into());
    }
    lines.extend(bullet("subdirectories with their own file", &target.children));
    lines.join("\n")
}

/// The prompt that asks for `target`'s context file. `child_summaries` pairs a child path with the
/// summary of its file, written earlier in this run.
pub fn file_prompt(target: &Target, child_summaries: &[(String, String)]) -> String {
    let root = target.path == ".";
    let place = if root {
        "the root of this repository".to_owned()
    } else {
        format!("the directory `{}` of this repository", target.path)
    };
    let mut out = vec![format!(
        "Write the AGENTS.md file for {place}. It briefs a coding agent that is about to work in this \
         directory: what it is for, how it is laid out, the commands to build, test and run it, the \
         conventions to follow, and the traps to avoid. Read the files you need with the read-only \
         tools; do not change any file."
    )];
    out.push(format!("What is already known:\n{}", facts(target)));
    if !child_summaries.is_empty() {
        let mut block =
            vec!["These subdirectories get their own file; link to them instead of repeating them:".to_owned()];
        for (path, summary) in child_summaries {
            block.push(format!("- `{path}`: {summary}"));
        }
        out.push(block.join("\n"));
    }
    if let (Action::Update, Some(existing)) = (target.action, &target.existing) {
        out.push(format!(
            "The current file follows. Keep what is still true, fix what is wrong, add what is missing:\n<current>\n{existing}\n</current>"
        ));
    }
    let limit = if root { 120 } else { 60 };
    out.push(format!(
        "Answer with the file's Markdown only (no code fence around it, no remarks before or after). \
         Start with a `# ` title naming {}. Add a section only when you have something specific to say. \
         {} Keep it under about {limit} lines.",
        if root { "the project" } else { "this directory" },
        if root {
            "Cover the whole project at a high level."
        } else {
            "Do not repeat what the files of parent directories already say; stay with this directory."
        },
    ));
    out.join("\n\n")
}

/// The model's answer as file content: trimmed, one wrapping code fence removed, newline-terminated.
pub fn clean_answer(answer: &str) -> String {
    let trimmed = answer.trim();
    let unfenced = trimmed
        .strip_prefix("```")
        .and_then(|rest| {
            let (first, body) = rest.split_once('\n')?;
            let lang = first.trim();
            (lang.is_empty() || lang.eq_ignore_ascii_case("markdown") || lang.eq_ignore_ascii_case("md"))
                .then_some(body)
        })
        .and_then(|body| body.trim_end().strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if unfenced.is_empty() {
        String::new()
    } else {
        format!("{unfenced}\n")
    }
}

/// The summary of a written file for its parent: title line and first paragraph, bounded.
pub fn summary(content: &str) -> String {
    let mut title = None;
    let mut paragraph = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if title.is_none() && line.starts_with('#') {
            title = Some(line.trim_start_matches('#').trim().to_owned());
            continue;
        }
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if line.starts_with('#') {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(line.to_owned());
    }
    let text = match title {
        Some(title) if paragraph.is_empty() => title,
        Some(title) => format!("{title} — {}", paragraph.join(" ")),
        None => paragraph.join(" "),
    };
    if text.chars().count() > SUMMARY_LIMIT {
        let cut: String = text.chars().take(SUMMARY_LIMIT - 1).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// The user message of `/init-deep`: the plan and the writing rules for the agent itself.
pub fn session_request(plan: &Plan) -> String {
    let mut out = vec![
        "Create AGENTS.md context files for this repository, one per directory listed below, \
         working from the deepest directory up so that each parent can link to its children \
         instead of repeating them. For each one, read what you need, then write the file with \
         your tools."
            .to_owned(),
    ];
    let todo: Vec<&Target> = plan
        .targets
        .iter()
        .filter(|t| t.action != Action::Keep)
        .collect();
    if todo.is_empty() {
        return "Every chosen directory of this repository already has a context file; nothing to \
                write. Run `/init-deep update` to revise them."
            .to_owned();
    }
    for target in &todo {
        let verb = if target.action == Action::Update {
            "revise the existing file"
        } else {
            "new file"
        };
        out.push(format!(
            "## `{}/AGENTS.md` ({verb})\n{}",
            target.path,
            facts(target)
        ));
    }
    let kept: Vec<String> = plan
        .targets
        .iter()
        .filter(|t| t.action == Action::Keep)
        .map(|t| format!("`{}`", t.path))
        .collect();
    if !kept.is_empty() {
        out.push(format!("Leave these existing files as they are: {}.", kept.join(", ")));
    }
    out.push(
        "Each file: Markdown, a `# ` title, sections only where there is something specific \
         (purpose, layout, commands, conventions, traps, where to start reading); the root file \
         covers the whole project in under about 120 lines, a subdirectory file stays with its \
         directory in under about 60 lines and does not repeat its parents."
            .to_owned(),
    );
    out.join("\n\n")
}
