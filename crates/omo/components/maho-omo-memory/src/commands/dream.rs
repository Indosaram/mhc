//! `/dream [--auto | --recent N | --conversation <ids> | --from transcript:<path>] [--to <doc>] [focus]`
//! Port of `components/memory/commands/dream.ts` at pin 77f3067f1.

use std::{path::Path, sync::Arc};

use maho_ext_api::ExtensionApi;
use memory_core::memfs::{ValidateMemoryPathOptions, validate_memory_path};

use super::args::{ParseCommandArgsOptions, FlagValue, parse_command_args_full};
use super::dream_staging::stage_dream_transcript;
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    DreamCommandOutcome, ManualDreamCommandRequest, MemoryCommandDeps, MemoryCommandIdentity,
    NotifyLevel,
};

const MODE_FLAGS: [&str; 4] = ["auto", "recent", "conversation", "from"];

pub async fn handle_dream(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let Some(sink) = &deps.dream else {
        return respond(
            ctx,
            "dreaming is not available in this session; restart with memory enabled",
            NotifyLevel::Error,
        );
    };

    let parsed = parse_command_args_full(args, ParseCommandArgsOptions { booleans: &["auto"] });
    let selected_modes: Vec<&str> = MODE_FLAGS
        .iter()
        .copied()
        .filter(|flag| parsed.has_flag(flag))
        .collect();
    if selected_modes.len() > 1 {
        return respond(
            ctx,
            "choose exactly one of --auto, --recent, --conversation, or --from",
            NotifyLevel::Error,
        );
    }

    let focus = parsed.positionals.join(" ").trim().to_owned();
    let focus = if focus.is_empty() { None } else { Some(focus.clone()) };

    let target_doc = match resolve_target_doc(
        &identity.identity_paths.repo,
        parsed.flag("to"),
    ) {
        Ok(target) => target,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let conversation_ids = match select_conversations(deps, ctx, &identity, &parsed, &focus).await {
        Ok(ids) => ids,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let request = ManualDreamCommandRequest {
        focus: focus.clone(),
        conversation_ids: conversation_ids.clone(),
        target_doc: target_doc.clone(),
    };
    let outcome = match sink.request(request).await {
        Ok(outcome) => outcome,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    match outcome {
        DreamCommandOutcome::Rejected { rejection } => {
            respond(ctx, dream_rejection(&rejection), NotifyLevel::Error)
        }
        DreamCommandOutcome::Fired { run_id, status } => {
            let text = if status == "active" {
                format!("dream run {run_id} reserved")
            } else {
                format!("dream request queued as {run_id}; another memory run is active")
            };
            respond(ctx, text, NotifyLevel::Info)
        }
    }
}

async fn select_conversations(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    identity: &MemoryCommandIdentity,
    parsed: &super::args::ParsedCommandArgs,
    focus: &Option<String>,
) -> Result<Option<Vec<String>>, String> {
    if parsed.has_flag("recent") {
        let recent_n = positive_integer(parsed.flag("recent"), "--recent")?;
        let settings = (deps.settings)()?;
        let settings = crate::dream_trigger_gates::resolve_dream_trigger_settings(
            &settings,
            Some(&identity.identity),
        )?;
        let options = crate::dream_selector::DreamSelectorOptions {
            transcripts_dir: &identity.identity_paths.transcripts,
            current_conversation_id: ctx.session_id.as_deref(),
            auto_select_max: settings.auto_select_max,
            auto_select_max_bytes: settings.auto_select_max_chars,
            now_ms: deps.now_ms() as f64,
        };
        let selected = crate::dream_selector::select_recent_dream_conversations(
            &options,
            recent_n,
            focus.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        if selected.conversation_ids.is_empty() {
            return Err("--recent found no unreflected conversations".to_owned());
        }
        return Ok(Some(selected.conversation_ids));
    }
    if parsed.has_flag("conversation") {
        return Ok(Some(conversation_list(parsed.flag("conversation"))?));
    }
    if parsed.has_flag("from") {
        let source = required_value(parsed.flag("from"), "--from")?;
        let Some(path) = source.strip_prefix("transcript:") else {
            return Err("--from expects transcript:<path> to a senpi session JSONL file".to_owned());
        };
        if path.is_empty() {
            return Err("--from expects transcript:<path> to a senpi session JSONL file".to_owned());
        }
        let staged = stage_dream_transcript(
            path,
            &identity.identity_paths.transcripts,
            &ctx.cwd,
        )?;
        return Ok(Some(vec![staged.conversation_id]));
    }
    Ok(None)
}

fn resolve_target_doc(
    repo_dir: &Path,
    value: Option<&FlagValue>,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = required_value(Some(value), "--to")?;
    if is_absolute(&raw) {
        return Err("--to must be a memory-repo-relative document path".to_owned());
    }
    let absolute = validate_memory_path(
        repo_dir,
        &raw,
        ValidateMemoryPathOptions { tool_path: false, field_name: "--to" },
    )
    .map_err(|_| {
        "--to must be a memory-repo-relative document path without traversal".to_owned()
    })?;
    let path = relative_to(repo_dir, &absolute);
    if !path.ends_with(".md") {
        return Err("--to must target a .md document in the memory repo".to_owned());
    }
    Ok(Some(path))
}

fn is_absolute(value: &str) -> bool {
    if Path::new(value).is_absolute() {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes.get(2).is_some_and(|separator| *separator == b'\\' || *separator == b'/')
}

fn relative_to(base: &Path, target: &Path) -> String {
    let relative = target.strip_prefix(base).unwrap_or(target);
    relative.to_string_lossy().replace('\\', "/")
}

fn positive_integer(value: Option<&FlagValue>, flag: &str) -> Result<usize, String> {
    let raw = required_value(value, flag)?;
    raw.trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{flag} expects a positive integer"))
}

fn conversation_list(value: Option<&FlagValue>) -> Result<Vec<String>, String> {
    let raw = required_value(value, "--conversation")?;
    let mut ids: Vec<String> = Vec::new();
    for id in raw.split(',') {
        let id = id.trim();
        if !id.is_empty() && !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_owned());
        }
    }
    if ids.is_empty() {
        return Err("--conversation expects one or more comma-separated ids".to_owned());
    }
    Ok(ids)
}

fn required_value(value: Option<&FlagValue>, flag: &str) -> Result<String, String> {
    match value.and_then(FlagValue::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.trim().to_owned()),
        _ => Err(format!("{flag} requires a value")),
    }
}

fn dream_rejection(rejection: &str) -> String {
    match rejection {
        "no_unreflected_content" => "dream found no unreflected content to process".to_owned(),
        "no_session" => "dreaming is not available without a bound memory session".to_owned(),
        other => format!("dream request was not started: {other}"),
    }
}

pub fn register_dream_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "dream",
        Some("Request a manual multi-conversation memory dream run.".to_owned()),
        Some(
            "[--auto | --recent N | --conversation <ids> | --from transcript:<path>] [--to <doc>] [focus]"
                .to_owned(),
        ),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_dream(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;
    use crate::dream_scoring::score_dream_candidate;
    use memory_core::journal::{
        entries::{TextTranscriptEntry, TranscriptEntry},
        store::{TranscriptJournal, TranscriptJournalOptions},
    };

    const NOW_MS: i64 = 1_786_363_200_000;

    fn harness() -> (tempfile::TempDir, MemoryCommandIdentity, FakeDeps) {
        let (root, identity) = temp_identity();
        let fake = fake_deps(
            Some(identity.clone()),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );
        (root, identity, fake)
    }

    fn write_conversation(
        transcripts_dir: &Path,
        conversation_id: &str,
        text: &str,
        captured_at: &str,
    ) {
        let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
            transcripts_dir.join(conversation_id),
        ));
        journal
            .append(&[TranscriptEntry::Text(TextTranscriptEntry::new(
                "user".to_owned(),
                text.to_owned(),
                captured_at.to_owned(),
                format!("{conversation_id}:line"),
                format!("{conversation_id}:message"),
            ))])
            .expect("append");
    }

    #[test]
    fn given_the_fixed_letta_parity_scoring_vector_when_scored_then_the_exact_operands_and_total_are_reproduced() {
        let input = crate::dream_scoring::DreamScoreInput {
            search_match_count: 3.0,
            steps_since_last_successful_reflection: 25.0,
            last_activity: "2026-08-03T12:00:00.000Z",
            distinct_source_count: 100.0,
            total_bytes: 30.0,
            target_bytes: 20.0,
            is_current: true,
            newest_message_covered: true,
            now_ms: NOW_MS as f64,
        };
        let (operands, score) = score_dream_candidate(&input);
        assert_eq!(operands.search_hits, 0.3);
        assert_eq!(operands.unreflected_steps, 0.5);
        assert_eq!(operands.recency, (-1.0f64).exp());
        assert_eq!(operands.source_count, 0.5);
        assert_eq!(operands.size_fit, 2.0 / 3.0);
        assert_eq!(operands.is_current, 1.0);
        assert_eq!(operands.penalty, 1.0);
        assert_eq!(score, 15.0 + 0.5 + (-1.0f64).exp() + 0.5 + 2.0 / 3.0);
    }

    #[tokio::test]
    async fn given_auto_and_focus_when_invoked_then_manual_dream_selection_is_delegated_without_explicit_ids() {
        let (_root, _identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_dream(&fake.deps, &context.ctx, "--auto commit discipline").await;

        let requests = dream_requests(&fake);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].focus.as_deref(), Some("commit discipline"));
        assert!(requests[0].conversation_ids.is_none());
        assert!(response.text.contains("dream-run-1"));
    }

    #[tokio::test]
    async fn given_three_conversations_when_recent_two_is_invoked_then_two_conversations_are_chosen_by_last_activity() {
        let (_root, identity, fake) = harness();
        write_conversation(&identity.identity_paths.transcripts, "older", "old", "2026-08-08T12:00:00.000Z");
        write_conversation(&identity.identity_paths.transcripts, "newest", "new", "2026-08-10T11:00:00.000Z");
        write_conversation(&identity.identity_paths.transcripts, "middle", "mid", "2026-08-09T12:00:00.000Z");
        let context = fake_command_context(FakeContextOptions::default());

        handle_dream(&fake.deps, &context.ctx, "--recent 2").await;

        let requests = dream_requests(&fake);
        assert_eq!(
            requests[0].conversation_ids,
            Some(vec!["newest".to_owned(), "middle".to_owned()])
        );
    }

    #[tokio::test]
    async fn given_explicit_conversation_ids_when_invoked_then_their_parsed_selection_is_submitted_unchanged() {
        let (_root, _identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        handle_dream(&fake.deps, &context.ctx, "--conversation sess-a,sess-b").await;

        let requests = dream_requests(&fake);
        assert_eq!(
            requests[0].conversation_ids,
            Some(vec!["sess-a".to_owned(), "sess-b".to_owned()])
        );
    }

    #[tokio::test]
    async fn given_valid_and_escaping_to_paths_when_invoked_then_only_the_memory_relative_document_is_accepted() {
        let (_root, identity, fake) = harness();
        seeded_repo(
            &identity,
            vec![seed("reference/style.md", "---\ndescription: Style\n---\nOriginal.\n")],
        );
        let context = fake_command_context(FakeContextOptions::default());

        handle_dream(&fake.deps, &context.ctx, "--conversation sess-a --to reference/style.md").await;
        let traversal =
            handle_dream(&fake.deps, &context.ctx, "--conversation sess-a --to ../escape").await;
        let absolute = handle_dream(&fake.deps, &context.ctx, "--conversation sess-a --to /abs").await;

        let requests = dream_requests(&fake);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].conversation_ids, Some(vec!["sess-a".to_owned()]));
        assert_eq!(requests[0].target_doc.as_deref(), Some("reference/style.md"));
        assert!(traversal.text.contains("memory-repo-relative"));
        assert!(absolute.text.contains("memory-repo-relative"));
    }

    #[tokio::test]
    async fn given_non_session_jsonl_when_imported_then_it_is_rejected_without_submitting_a_dream() {
        let (_root, identity, fake) = harness();
        let transcript = identity.identity_paths.transcripts.join("not-session.jsonl");
        std::fs::create_dir_all(&identity.identity_paths.transcripts).expect("transcripts");
        std::fs::write(
            &transcript,
            format!("{}\n", serde_json::json!({ "kind": "user", "text": "not senpi" })),
        )
        .expect("write");
        let context = fake_command_context(FakeContextOptions::default());

        let staging_error = stage_dream_transcript(
            transcript.to_string_lossy().as_ref(),
            &identity.identity_paths.transcripts,
            &identity.identity_paths.transcripts,
        )
        .err();
        let response = handle_dream(
            &fake.deps,
            &context.ctx,
            &format!("--from transcript:{}", transcript.display()),
        )
        .await;

        assert!(staging_error.is_some());
        assert!(dream_requests(&fake).is_empty());
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
        assert_eq!(response.text, staging_error.unwrap());
    }

    #[tokio::test]
    async fn given_a_senpi_jsonl_transcript_with_a_malformed_line_when_imported_twice_then_message_ids_are_staged_once() {
        let (_root, identity, fake) = harness();
        let agent_dir = identity.identity_paths.transcripts.parent().unwrap_or(Path::new("/")).to_path_buf();
        let transcript = agent_dir.join("source.jsonl");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let file_mtime = "2026-08-10T10:02:00.000Z";
        std::fs::write(
            &transcript,
            [
                serde_json::json!({ "type": "session", "id": "source-session", "timestamp": "2026-08-10T10:00:00.000Z" }).to_string(),
                serde_json::json!({ "type": "message", "id": "message-1", "timestamp": "2026-08-10T10:01:00.000Z", "message": { "role": "user", "content": [{ "type": "text", "text": "remember tabs" }] } }).to_string(),
                "{ malformed".to_owned(),
                serde_json::json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": "tabs are preferred" }] } }).to_string(),
            ]
            .join("\n"),
        )
        .expect("write transcript");
        let context = fake_command_context(FakeContextOptions::default());

        let first = stage_dream_transcript(
            transcript.to_string_lossy().as_ref(),
            &identity.identity_paths.transcripts,
            &agent_dir,
        )
        .expect("first staging");
        let second = stage_dream_transcript(
            transcript.to_string_lossy().as_ref(),
            &identity.identity_paths.transcripts,
            &agent_dir,
        )
        .expect("second staging");
        handle_dream(
            &fake.deps,
            &context.ctx,
            &format!("--from transcript:{}", transcript.display()),
        )
        .await;
        let rows = TranscriptJournal::new(TranscriptJournalOptions::new(
            identity.identity_paths.transcripts.join(&first.conversation_id),
        ))
        .read_entries()
        .expect("rows");

        let first_derived = super::super::dream_staging::sha1_hex(
            format!("{}:1:remember tabs", transcript.display()).as_bytes(),
        )[..16]
            .to_owned();
        let second_derived = super::super::dream_staging::sha1_hex(
            format!("{}:3:tabs are preferred", transcript.display()).as_bytes(),
        )[..16]
            .to_owned();
        let staged: std::collections::BTreeSet<String> =
            first.staged_message_ids.iter().cloned().collect();
        assert_eq!(
            staged,
            [first_derived.clone(), second_derived.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert!(second.staged_message_ids.is_empty());
        let skipped: std::collections::BTreeSet<String> =
            second.skipped_message_ids.iter().cloned().collect();
        assert_eq!(skipped, staged);
        assert_eq!(dream_requests(&fake)[0].conversation_ids, Some(vec![first.conversation_id.clone()]));
        let unique_rows: std::collections::BTreeSet<String> =
            rows.iter().map(|row| row.source_message_id().to_owned()).collect();
        assert_eq!(unique_rows.len(), 2);
        let _ = file_mtime;
    }
}
