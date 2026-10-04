//! `/search <query> [--include-hidden]` — FTS-lite scan over senpi session JSONL.
//! Port of `components/memory/commands/search.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use memory_core::search::{
    SearchOptions, SearchResult, SenpiSessionProvider, SenpiSessionProviderOptions, parse_query,
    search_transcripts, searchable_text,
};

use super::args::{ParseCommandArgsOptions, parse_command_args_full};
use super::types::{
    command_context_from, finish, respond, CommandContext, CommandResponse, MemoryCommandDeps,
    NotifyLevel,
};

const SNIPPET_BEFORE: usize = 60;
const SNIPPET_AFTER: usize = 120;

fn highlight(snippet: &str, needles: &[String]) -> String {
    let mut highlighted = snippet.to_owned();
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        highlighted = highlight_insensitive(&highlighted, needle);
    }
    highlighted
}

fn highlight_insensitive(haystack: &str, needle: &str) -> String {
    let lower_haystack = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(relative) = lower_haystack[cursor..].find(&lower_needle) {
        let start = cursor + relative;
        let end = start + lower_needle.len();
        out.push_str(&haystack[cursor..start]);
        out.push_str("**");
        out.push_str(&haystack[start..end]);
        out.push_str("**");
        cursor = end;
    }
    out.push_str(&haystack[cursor..]);
    out
}

fn build_snippet(result: &SearchResult, needles: &[String]) -> String {
    let haystack: String = searchable_text(&result.document)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let lowered = haystack.to_lowercase();
    let mut index: Option<usize> = None;
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        if let Some(found) = lowered.find(&needle.to_lowercase()) {
            index = Some(index.map_or(found, |current| current.min(found)));
        }
    }
    let anchor = index.unwrap_or(0);
    let start = anchor.saturating_sub(SNIPPET_BEFORE);
    let end = (anchor + SNIPPET_AFTER).min(haystack.len());
    let core = safe_slice(&haystack, start, end);
    let prefix = if start > 0 { "..." } else { "" };
    let suffix = if end < haystack.len() { "..." } else { "" };
    format!("{prefix}{}{suffix}", highlight(core, needles))
}

fn safe_slice(value: &str, start: usize, end: usize) -> &str {
    let mut start = start.min(value.len());
    let mut end = end.min(value.len());
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    while end > start && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[start..end]
}

fn resolve_sessions_dir(deps: &MemoryCommandDeps, ctx: &CommandContext) -> Option<std::path::PathBuf> {
    deps.sessions_root(ctx)
}

pub async fn handle_search(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let parsed = parse_command_args_full(
        args,
        ParseCommandArgsOptions { booleans: &["include-hidden"] },
    );
    let query = parsed.positionals.join(" ").trim().to_owned();
    if query.is_empty() {
        return respond(ctx, "usage: /search <query> [--include-hidden]", NotifyLevel::Error);
    }

    let sessions_dir = resolve_sessions_dir(deps, ctx);
    let Some(sessions_dir) = sessions_dir.filter(|dir| dir.exists()) else {
        return respond(
            ctx,
            format!(
                "no senpi sessions directory found at {}; start a session first or check the agent directory",
                resolve_sessions_dir(deps, ctx)
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_owned())
            ),
            NotifyLevel::Error,
        );
    };

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir,
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });
    let options = SearchOptions {
        include_hidden: Some(parsed.flag_is_true("include-hidden")),
        ..Default::default()
    };
    let results = search_transcripts(&provider, &query, &options);
    if results.is_empty() {
        return respond(ctx, format!("No matches for \"{query}\"."), NotifyLevel::Info);
    }

    let parsed_query = parse_query(&query);
    let mut needles = parsed_query.phrases.clone();
    needles.extend(parsed_query.terms.clone());
    let mut lines = vec![
        format!(
            "# Search: \"{query}\" — {} result{}",
            results.len(),
            if results.len() == 1 { "" } else { "s" }
        ),
        String::new(),
    ];
    for (index, result) in results.iter().enumerate() {
        lines.push(format!(
            "{}. session {} · {} · {}",
            index + 1,
            result.conversation_id,
            result.created_at,
            result.document.message_type.as_deref().unwrap_or("unknown")
        ));
        lines.push(format!("   {}", build_snippet(result, &needles)));
        lines.push(format!("   entry {}", result.message_id));
        lines.push(String::new());
    }
    let text = lines.join("\n").trim_end().to_owned();
    respond(ctx, text, NotifyLevel::Info)
}

pub fn register_search_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "search",
        Some("Search past session transcripts and show scored, numbered results.".to_owned()),
        Some("<query> [--include-hidden]".to_owned()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_search(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    fn write_session(
        sessions_dir: &std::path::Path,
        dir_name: &str,
        session_id: &str,
        messages: &[(&str, &str, &str, &str)],
    ) -> std::path::PathBuf {
        let dir = sessions_dir.join(dir_name);
        std::fs::create_dir_all(&dir).expect("session dir");
        let mut lines = vec![serde_json::json!({
            "type": "session",
            "id": session_id,
            "timestamp": messages.first().map(|message| message.3),
        })
        .to_string()];
        for (id, role, text, timestamp) in messages {
            lines.push(
                serde_json::json!({
                    "type": "message",
                    "id": id,
                    "timestamp": timestamp,
                    "message": { "role": role, "content": [{ "type": "text", "text": text }] },
                })
                .to_string(),
            );
        }
        let file_path = dir.join(format!("{session_id}.jsonl"));
        std::fs::write(&file_path, format!("{}\n", lines.join("\n"))).expect("session file");
        file_path
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf, FakeDeps) {
        let root = tempfile::tempdir().expect("sessions root");
        let sessions_dir = root.path().to_path_buf();
        write_session(
            &sessions_dir,
            "sess-a",
            "sess-a",
            &[
                ("msg-1", "user", "how do I run the linter before committing", "2026-08-01T10:00:01.000Z"),
                ("msg-2", "assistant", "run bun run lint first, then commit", "2026-08-01T10:00:02.000Z"),
            ],
        );
        write_session(
            &sessions_dir,
            "sess-b",
            "sess-b",
            &[("msg-9", "user", "tabs versus spaces preference", "2026-08-02T09:00:00.000Z")],
        );
        let hidden = write_session(
            &sessions_dir,
            "sess-hidden",
            "sess-hidden",
            &[("msg-h", "user", "linter notes from a hidden session", "2026-08-03T09:00:00.000Z")],
        );
        std::fs::write(format!("{}.archived", hidden.display()), "").expect("archived marker");

        let (_root, identity) = temp_identity();
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { sessions_dir: Some(sessions_dir), ..Default::default() },
        );
        (root, root.path().to_path_buf(), fake)
    }

    #[tokio::test]
    async fn given_sessions_on_disk_when_queried_then_numbered_results_carry_session_date_role_snippet_and_entry() {
        let (_root, _dir, fake) = setup();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_search(&fake.deps, &context.ctx, "linter").await;

        assert!(response.text.contains("1. session sess-a · 2026-08-01T10:00:01.000Z · user"));
        assert!(response.text.contains("**linter**"));
        assert!(response.text.contains("entry msg-1"));
        assert!(!response.text.contains("sess-hidden"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_a_hidden_session_when_queried_with_include_hidden_then_hidden_results_appear() {
        let (_root, _dir, fake) = setup();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_search(&fake.deps, &context.ctx, "linter --include-hidden").await;

        assert!(response.text.contains("sess-hidden"));
    }

    #[tokio::test]
    async fn given_no_matching_documents_when_queried_then_a_no_results_message_is_returned() {
        let (_root, _dir, fake) = setup();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_search(&fake.deps, &context.ctx, "zzzznotfound").await;

        assert!(response.text.contains("No matches for \"zzzznotfound\""));
    }

    #[tokio::test]
    async fn given_an_empty_query_when_invoked_then_a_usage_error_is_returned() {
        let (_root, _dir, fake) = setup();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_search(&fake.deps, &context.ctx, "  ").await;

        assert!(response.text.contains("usage: /search <query>"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_a_missing_sessions_directory_when_invoked_then_an_actionable_error_is_returned() {
        let fake = fake_deps(
            None,
            FakeDepsOverrides {
                sessions_dir: Some(std::path::PathBuf::from("/nonexistent/sessions")),
                ..Default::default()
            },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_search(&fake.deps, &context.ctx, "linter").await;

        assert!(response.text.contains("no senpi sessions directory"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
