//! `/facts -- read-only view of the facts pipeline, plus the ONLY manual unpark path.
//! Port of `components/memory/commands/facts.ts` at pin 77f3067f1.

use std::{collections::BTreeSet, sync::Arc};

use maho_ext_api::ExtensionApi;
use memory_core::facts::{
    failures_backoff::FactsFailureFilter,
    failures_store::{FactsFailureStore, FactsFailureStoreError, FactsFailureStoreOptions},
};

use super::args::{ParseCommandArgsOptions, parse_command_args_full};
use super::facts_status::{ReadFactsOverviewInput, format_facts_status, read_facts_overview};
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, MemoryCommandIdentity, NotifyLevel,
};

fn conversations_in(records: &[memory_core::facts::failures_schema::FactsFailureRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.conversation_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn run_retry(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    identity: &MemoryCommandIdentity,
    conversation_id: Option<String>,
) -> CommandResponse {
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: identity.identity_paths.clone(),
        now: Some(Arc::new({
            let now_ms = deps.now_ms();
            move || now_ms
        })),
        lock_wait_ms: None,
    });
    let before = match store.read_failures() {
        Ok(before) => before,
        Err(error) => {
            let detail = match &error {
                FactsFailureStoreError::Corrupt(corrupt) => corrupt.to_string(),
                other => other.to_string(),
            };
            return respond(
                ctx,
                format!(
                    "cannot retry: the failure ledger is unreadable ({detail}); repair or remove failures.json first"
                ),
                NotifyLevel::Error,
            );
        }
    };

    let matching: Vec<_> = before
        .entries
        .iter()
        .filter(|record| {
            conversation_id
                .as_ref()
                .is_none_or(|id| id == &record.conversation_id)
        })
        .cloned()
        .collect();
    if matching.is_empty() {
        let scope = match &conversation_id {
            None => "the facts ledger".to_owned(),
            Some(id) => format!("conversation {id}"),
        };
        return respond(
            ctx,
            format!("no failure records to clear for {scope}; nothing was retried"),
            NotifyLevel::Info,
        );
    }

    let filter = FactsFailureFilter {
        conversation_id: conversation_id.clone(),
        end_message_id: None,
    };
    let removed = match store.clear_for_retry(&filter) {
        Ok(removed) => removed,
        Err(error) => return respond(ctx, error.to_string(), NotifyLevel::Error),
    };
    if let Some(retry) = &deps.facts_retry
        && let Err(error) = retry(identity.identity.clone()).await
    {
        return respond(ctx, error, NotifyLevel::Error);
    }
    let names = conversations_in(&matching);
    respond(
        ctx,
        format!(
            "cleared {removed} record{} for {}; one launch attempt was triggered",
            if removed == 1 { "" } else { "s" },
            names.join(", ")
        ),
        NotifyLevel::Info,
    )
}

pub async fn handle_facts(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let parsed = parse_command_args_full(args, ParseCommandArgsOptions::default());
    let conversation_id = parsed.flag_value("conversation").map(str::to_owned);

    if parsed.positionals.first().map(String::as_str) == Some("retry") {
        return run_retry(deps, ctx, &identity, conversation_id).await;
    }
    if let Some(first) = parsed.positionals.first() {
        return respond(
            ctx,
            format!("unknown /facts subcommand \"{first}\"; use /facts or /facts retry"),
            NotifyLevel::Error,
        );
    }

    let overview = read_facts_overview(ReadFactsOverviewInput {
        identity_paths: &identity.identity_paths,
        now_ms: deps.now_ms(),
    });
    respond(
        ctx,
        format_facts_status(&identity.identity, &overview),
        NotifyLevel::Info,
    )
}

pub fn register_facts_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "facts",
        Some(
            "Show facts-extraction queue/backoff state, or manually retry parked batches."
                .to_owned(),
        ),
        Some("[retry [--conversation <id>]]".to_owned()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_facts(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;
    use memory_core::facts::{
        failures_backoff::FactsFailureTarget,
        failures_schema::FactsFailureReason,
        failures_store::RecordFailureRequest,
        schema::facts_queue_paths,
    };

    const NOW_MS: i64 = 1_786_968_000_000;

    async fn seed_failures(identity: &MemoryCommandIdentity) -> FactsFailureStore {
        let store = FactsFailureStore::new(FactsFailureStoreOptions {
            identity_paths: identity.identity_paths.clone(),
            now: Some(Arc::new(|| NOW_MS)),
            lock_wait_ms: None,
        });
        for attempt in 0..5 {
            store
                .record_failure(RecordFailureRequest {
                    targets: vec![FactsFailureTarget {
                        conversation_id: "conv-a".to_owned(),
                        end_message_id: "msg-a".to_owned(),
                        end_snapshot_line: 4,
                    }],
                    failure_id: format!("run-a-{attempt}"),
                    reason: FactsFailureReason::ChildExit,
                    detail: Some("child exited with code 1".to_owned()),
                })
                .expect("record a");
        }
        store
            .record_failure(RecordFailureRequest {
                targets: vec![FactsFailureTarget {
                    conversation_id: "conv-b".to_owned(),
                    end_message_id: "msg-b".to_owned(),
                    end_snapshot_line: 7,
                }],
                failure_id: "run-b-0".to_owned(),
                reason: FactsFailureReason::DeadlineExceeded,
                detail: None,
            })
            .expect("record b");
        store
    }

    fn seed_queue_artifacts(identity: &MemoryCommandIdentity) -> Vec<std::path::PathBuf> {
        let layout = facts_queue_paths(&identity.identity_paths);
        std::fs::create_dir_all(&layout.cursor_dir).expect("cursor dir");
        let entry_path = layout.queue_dir.join("20260816T115900000Z-aaaaaaaaaaaa-bbbbbbbb.json");
        std::fs::write(&entry_path, "{\n  \"version\": 1,\n  \"entries\": []\n}\n").expect("entry");
        std::fs::write(&layout.consumed_path, "{\n  \"version\": 1,\n  \"consumed\": {}\n}\n")
            .expect("consumed");
        let cursor_path = layout.cursor_path("conv-a");
        std::fs::write(&cursor_path, "{\n  \"version\": 1,\n  \"enqueued_through_snapshot_line\": 4\n}\n")
            .expect("cursor");
        vec![entry_path, layout.consumed_path, cursor_path]
    }

    fn snapshot(paths: &[std::path::PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| std::fs::read_to_string(path).expect("read"))
            .collect()
    }

    async fn invoke_facts(fake: &FakeDeps, args: &str) -> CommandResponse {
        let context = fake_command_context(FakeContextOptions::default());
        handle_facts(&fake.deps, &context.ctx, args).await
    }

    #[tokio::test]
    async fn given_parked_and_backoff_records_when_invoked_then_the_status_view_reports_both_plus_queue_depth() {
        let (_root, identity) = temp_identity();
        seed_failures(&identity).await;
        seed_queue_artifacts(&identity);
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );

        let response = invoke_facts(&fake, "").await;

        assert!(response.text.contains("parked: 1"));
        assert!(response.text.contains("backoff: 1"));
        assert!(response.text.contains("conv-a"));
        assert!(response.text.contains("child exited with code 1"));
        assert!(response.text.contains("/facts retry"));
    }

    #[tokio::test]
    async fn given_a_corrupt_failures_json_when_invoked_then_the_status_view_says_corrupt_rather_than_zeros() {
        let (_root, identity) = temp_identity();
        let layout = facts_queue_paths(&identity.identity_paths);
        std::fs::create_dir_all(&layout.queue_dir).expect("queue dir");
        std::fs::write(&layout.failures_path, "{ not json").expect("corrupt");
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );

        let response = invoke_facts(&fake, "").await;

        assert!(response.text.contains("UNREADABLE"));
        assert!(!response.text.contains("parked: 0"));
        assert!(!response.text.contains("backoff: 0"));
    }

    #[tokio::test]
    async fn given_records_for_two_conversations_when_retrying_one_then_only_its_records_are_cleared() {
        let (_root, identity) = temp_identity();
        let store = seed_failures(&identity).await;
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );

        let response = invoke_facts(&fake, "retry --conversation conv-a").await;

        let remaining = store.read_failures().expect("read");
        let ids: Vec<String> = remaining.entries.iter().map(|r| r.conversation_id.clone()).collect();
        assert_eq!(ids, vec!["conv-b".to_owned()]);
        assert!(response.text.contains("conv-a"));
        assert!(response.text.contains("1 record"));
    }

    #[tokio::test]
    async fn given_queue_files_and_watermarks_when_retrying_then_every_queue_artifact_is_byte_identical() {
        let (_root, identity) = temp_identity();
        seed_failures(&identity).await;
        let paths = seed_queue_artifacts(&identity);
        let before = snapshot(&paths);
        let layout = facts_queue_paths(&identity.identity_paths);
        let mut names_before: Vec<String> = std::fs::read_dir(&layout.queue_dir)
            .expect("queue dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names_before.sort();
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );

        invoke_facts(&fake, "retry").await;

        assert_eq!(snapshot(&paths), before);
        let mut names_after: Vec<String> = std::fs::read_dir(&layout.queue_dir)
            .expect("queue dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names_after.sort();
        assert_eq!(names_after, names_before);
    }

    #[tokio::test]
    async fn given_parked_records_when_retrying_without_a_filter_then_all_conversations_are_named_and_one_launch_triggers() {
        let (_root, identity) = temp_identity();
        let store = seed_failures(&identity).await;
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );

        let response = invoke_facts(&fake, "retry").await;

        assert!(store.read_failures().expect("read").entries.is_empty());
        assert!(response.text.contains("conv-a"));
        assert!(response.text.contains("conv-b"));
        assert_eq!(retry_triggers(&fake), vec!["reconcile".to_owned()]);
    }

    #[tokio::test]
    async fn given_no_records_for_the_requested_conversation_when_retrying_then_it_is_a_noop_with_a_clear_message() {
        let (_root, identity) = temp_identity();
        let store = seed_failures(&identity).await;
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { now_ms: Some(NOW_MS), ..Default::default() },
        );

        let response = invoke_facts(&fake, "retry --conversation unknown-conv").await;

        assert_eq!(store.read_failures().expect("read").entries.len(), 2);
        assert!(response.text.contains("no failure records"));
        assert!(response.text.contains("unknown-conv"));
        assert!(retry_triggers(&fake).is_empty());
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_an_actionable_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());

        let response = invoke_facts(&fake, "").await;

        assert!(response.text.contains("not bound"));
    }
}
