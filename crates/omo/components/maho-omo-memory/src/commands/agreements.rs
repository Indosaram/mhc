//! `/agreements`: the person's own surface over `system/agreements.md` (pin `commands/agreements.ts`).
//! It is the only writer that may create, reword, acknowledge, or drop an item written by the person
//! (`by: human`); automation is refused those edits by memory-core's agreements guard.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use memory_core::agreements::{
    AGREEMENTS_PATH, AgreementItem, AgreementScope, AgreementStatus, AgreementsEdit,
    ack_agreements, add_human_agreement, agreement_items_of, agreement_text_problem,
    clean_agreement_text, drop_agreements, read_agreements_worktree, render_agreements_file,
    sort_agreements,
};
use memory_core::git::{GitCommitAuthor, GitMemoryRepo};
use memory_core::locks::{
    AcquireLockOptions, CreateLockRecordOptions, create_lock_record, memory_writer_lock_path,
    with_lock,
};

use super::repo::{has_git_repo, open_repo};
use super::types::{
    CommandContext, CommandResponse, MemoryCommandDeps, MemoryCommandIdentity, NotifyLevel,
    command_context_from, finish, require_identity, respond,
};

pub const AGREEMENTS_USAGE: &str =
    "usage: /agreements [list] | add [--project] <agreement> | ack <id>... | drop <id>...";
const NOT_INITIALIZED: &str = "memory repository is not initialized yet; run /memfs init first";

pub async fn handle_agreements(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    if !has_git_repo(&identity) {
        return respond(ctx, NOT_INITIALIZED, NotifyLevel::Error);
    }
    let mut words = args.split_whitespace();
    let verb = words.next().unwrap_or("list");
    let rest: Vec<&str> = words.collect();
    let repo = match open_repo(deps, &identity) {
        Ok(repo) => repo,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    match verb {
        "list" => match read_items(&repo) {
            Ok(items) => respond(ctx, render_list(&items), NotifyLevel::Info),
            Err(error) => respond(ctx, error, NotifyLevel::Error),
        },
        "add" => add(deps, ctx, &identity, &repo, &rest),
        "ack" | "drop" if !rest.is_empty() => {
            let ids: Vec<String> = rest.iter().map(|id| (*id).to_owned()).collect();
            let missing = format!("no agreement has id {}", ids.join(", "));
            let reason = if ids.len() == 1 {
                format!("{verb} agreement {}", ids[0])
            } else {
                format!("{verb} agreements")
            };
            let is_ack = verb == "ack";
            write(deps, ctx, &identity, &repo, &reason, &missing, &|items| {
                if is_ack {
                    ack_agreements(items, &ids)
                } else {
                    drop_agreements(items, &ids)
                }
            })
        }
        _ => respond(ctx, AGREEMENTS_USAGE, NotifyLevel::Error),
    }
}

fn read_items(repo: &GitMemoryRepo) -> Result<Vec<AgreementItem>, String> {
    let existing = read_agreements_worktree(&repo.dir).map_err(|error| error.to_string())?;
    Ok(agreement_items_of(existing.as_deref()))
}

fn add(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    identity: &MemoryCommandIdentity,
    repo: &GitMemoryRepo,
    words: &[&str],
) -> CommandResponse {
    let project = words.first() == Some(&"--project");
    let text = clean_agreement_text(&words[usize::from(project)..].join(" "));
    if let Some(problem) = agreement_text_problem(&text) {
        return respond(
            ctx,
            format!("{problem}\n{AGREEMENTS_USAGE}"),
            NotifyLevel::Error,
        );
    }
    let today = chrono::DateTime::from_timestamp_millis(deps.now_ms()).map_or_else(
        || "1970-01-01".to_owned(),
        |at| at.format("%Y-%m-%d").to_string(),
    );
    let scope = if project {
        AgreementScope::Project
    } else {
        AgreementScope::Global
    };
    write(
        deps,
        ctx,
        identity,
        repo,
        "add agreement",
        "nothing to change",
        &|items| {
            Some(AgreementsEdit {
                items: add_human_agreement(items, &text, scope, &today),
                unknown: Vec::new(),
            })
        },
    )
}

fn write(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    identity: &MemoryCommandIdentity,
    repo: &GitMemoryRepo,
    reason: &str,
    missing: &str,
    change: &dyn Fn(&[AgreementItem]) -> Option<AgreementsEdit>,
) -> CommandResponse {
    let author = GitCommitAuthor {
        agent_id: identity.identity.clone(),
        author_name: "Human".into(),
        author_email: None,
    };
    let message = match ctx.session_id.as_deref() {
        Some(session) => {
            format!("{reason}\n\nOmo-Writer: human-command\nOmo-Session: {session}\nOmo-Turn: 0")
        }
        None => reason.to_owned(),
    };
    let record = match create_lock_record(
        &format!("/agreements ({})", identity.identity),
        CreateLockRecordOptions::default(),
    ) {
        Ok(record) => record,
        Err(error) => return respond(ctx, format!("{error:?}"), NotifyLevel::Error),
    };
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(5_000),
        ..Default::default()
    };
    let outcome = with_lock(
        &memory_writer_lock_path(&identity.identity_paths.locks),
        &record,
        &options,
        || -> Result<Option<(String, String)>, String> {
            let existing =
                read_agreements_worktree(&repo.dir).map_err(|error| error.to_string())?;
            let Some(edit) = change(&agreement_items_of(existing.as_deref())) else {
                return Ok(None);
            };
            let next = edit.items;
            let note = if edit.unknown.is_empty() {
                String::new()
            } else {
                format!("\nno agreement has id {}", edit.unknown.join(", "))
            };
            std::fs::create_dir_all(repo.dir.join("system")).map_err(|error| error.to_string())?;
            let rendered = render_agreements_file(existing.as_deref(), &next)
                .map_err(|error| format!("{error:?}"))?;
            let message = if memory_core::tools::claim_frontmatter::stripped_legacy_user_header(
                existing.as_deref(),
                &rendered,
            ) {
                format!(
                    "{message}\n{}: {AGREEMENTS_PATH}",
                    memory_core::tools::claim_frontmatter::PROVENANCE_STRIPPED_TRAILER
                )
            } else {
                message.clone()
            };
            std::fs::write(repo.dir.join(AGREEMENTS_PATH), rendered)
                .map_err(|error| error.to_string())?;
            let committed = repo
                .commit_write(&[AGREEMENTS_PATH], &message, &author)
                .map_err(|error| error.to_string())?;
            Ok(Some((committed.sha, note)))
        },
    );
    match outcome {
        Ok(None) => respond(ctx, missing, NotifyLevel::Error),
        Ok(Some((sha, note))) => {
            (deps.bust_prompt_cache)();
            let short: String = sha.chars().take(7).collect();
            let listing = read_items(repo)
                .map(|items| render_list(&items))
                .unwrap_or_else(|error| error);
            respond(
                ctx,
                format!("/agreements {reason} committed ({short}).{note}\n{listing}"),
                NotifyLevel::Info,
            )
        }
        Err(error) => respond(ctx, error.to_string(), NotifyLevel::Error),
    }
}

/// Needs-human items first, each as three lines.
pub fn render_list(items: &[AgreementItem]) -> String {
    if items.is_empty() {
        return "no agreements yet; /agreements add <text> records one in your words".to_owned();
    }
    let ordered = sort_agreements(items);
    let flagged = |item: &&AgreementItem| item.status == AgreementStatus::NeedsHuman;
    ordered
        .iter()
        .filter(flagged)
        .chain(ordered.iter().filter(|item| !flagged(item)))
        .map(|item| {
            format!(
                "{}  [{}] [{}] broken x{}{}\n  {}\n  since {}, last {}",
                item.id,
                item.scope.as_str(),
                item.by.as_str(),
                item.n,
                if item.status == AgreementStatus::NeedsHuman {
                    "  NEEDS-HUMAN"
                } else {
                    ""
                },
                item.text,
                item.since.as_deref().unwrap_or("?"),
                item.last.as_deref().unwrap_or("never"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn register_agreements_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "agreements",
        Some("List, add, acknowledge, or drop your standing agreements with the agent.".to_owned()),
        Some("[list] | add [--project] <text> | ack <id>... | drop <id>...".to_owned()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_agreements(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    fn item(
        id: &str,
        by: memory_core::agreements::AgreementAuthor,
        n: u64,
        status: AgreementStatus,
    ) -> AgreementItem {
        AgreementItem {
            id: id.into(),
            scope: AgreementScope::Global,
            by,
            text: format!("text {id}"),
            n,
            since: Some("2026-10-01".into()),
            last: None,
            status,
        }
    }

    #[test]
    fn list_puts_needs_human_items_first_and_marks_them() {
        use memory_core::agreements::AgreementAuthor::{Agent, Human};
        let rendered = render_list(&[
            item("aaaaaaaaaa", Human, 0, AgreementStatus::Active),
            item("bbbbbbbbbb", Agent, 4, AgreementStatus::NeedsHuman),
        ]);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(
            lines[0],
            "bbbbbbbbbb  [global] [agent] broken x4  NEEDS-HUMAN"
        );
        assert_eq!(lines[2], "  since 2026-10-01, last never");
        assert_eq!(lines[3], "aaaaaaaaaa  [global] [human] broken x0");
    }

    #[test]
    fn an_empty_ledger_says_how_to_add() {
        assert_eq!(
            render_list(&[]),
            "no agreements yet; /agreements add <text> records one in your words"
        );
    }

    #[tokio::test]
    async fn without_a_repository_the_command_points_at_memfs_init() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        let response = handle_agreements(&fake.deps, &context.ctx, "list").await;
        assert!(response.is_error());
        assert_eq!(response.text, NOT_INITIALIZED);
    }

    #[tokio::test]
    async fn stripped_legacy_header_is_recorded_in_commit_provenance() {
        // Legacy receipts use JSON-encoded string scalars, as in upstream
        // memory-core/src/facts/extraction-legacy-header.test.ts.
        let (_root, identity) = temp_identity();
        seeded_repo(
            &identity,
            vec![seed(
                "system/agreements.md",
                "---\ndescription: Agreements\nprovenance: \"user said yes\"\n---\n\n",
            )],
        );
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        let response = handle_agreements(&fake.deps, &context.ctx, "add never force push").await;
        assert!(!response.is_error(), "{}", response.text);
        let repo = super::open_repo(&fake.deps, &identity).unwrap();
        let history = repo.log(None).unwrap();
        assert_eq!(
            history[0]
                .trailers
                .get("Omo-Provenance-Stripped")
                .map(String::as_str),
            Some(AGREEMENTS_PATH)
        );
    }

    #[tokio::test]
    async fn add_ack_and_drop_round_trip_through_the_ledger() {
        let (_root, identity) = temp_identity();
        seeded_repo(
            &identity,
            vec![seed(
                "system/persona.md",
                "---\ndescription: Persona\n---\nbody\n",
            )],
        );
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        let added =
            handle_agreements(&fake.deps, &context.ctx, "add --project never force push").await;
        assert!(!added.is_error(), "{}", added.text);
        assert!(
            added.text.contains("[project] [human]") && added.text.contains("never force push")
        );
        let id = added
            .text
            .lines()
            .find(|line| line.contains("[project]"))
            .and_then(|line| line.split_whitespace().next())
            .unwrap()
            .to_owned();
        let missing = handle_agreements(&fake.deps, &context.ctx, "ack 0000000000").await;
        assert_eq!(missing.text, "no agreement has id 0000000000");
        let dropped = handle_agreements(&fake.deps, &context.ctx, &format!("drop {id}")).await;
        assert!(!dropped.is_error(), "{}", dropped.text);
        assert!(dropped.text.contains("no agreements yet"));
        let usage = handle_agreements(&fake.deps, &context.ctx, "ack").await;
        assert_eq!(usage.text, AGREEMENTS_USAGE);
    }

    async fn add_two(fake: &FakeDeps, context: &FakeContext) -> Vec<String> {
        for text in ["run the tests", "never force push"] {
            let added = handle_agreements(&fake.deps, &context.ctx, &format!("add {text}")).await;
            assert!(!added.is_error(), "{}", added.text);
        }
        let listing = handle_agreements(&fake.deps, &context.ctx, "list").await;
        let ids: Vec<String> = listing
            .text
            .lines()
            .filter(|line| line.contains("[human]"))
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect();
        assert_eq!(ids.len(), 2, "{}", listing.text);
        ids
    }

    #[tokio::test]
    async fn drop_takes_several_ids_in_one_commit() {
        let (_root, identity) = temp_identity();
        seeded_repo(
            &identity,
            vec![seed(
                "system/persona.md",
                "---\ndescription: Persona\n---\nbody\n",
            )],
        );
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        let ids = add_two(&fake, &context).await;

        let dropped = handle_agreements(
            &fake.deps,
            &context.ctx,
            &format!("drop {} {}", ids[0], ids[1]),
        )
        .await;
        assert!(!dropped.is_error(), "{}", dropped.text);
        assert!(
            dropped.text.contains("drop agreements committed"),
            "{}",
            dropped.text
        );
        assert!(
            dropped.text.contains("no agreements yet"),
            "{}",
            dropped.text
        );
        for id in &ids {
            assert!(!dropped.text.contains(id.as_str()), "{}", dropped.text);
        }
    }

    #[tokio::test]
    async fn drop_applies_the_known_ids_and_names_the_unknown_ones() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, vec![]);
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        let ids = add_two(&fake, &context).await;

        let partial = handle_agreements(
            &fake.deps,
            &context.ctx,
            &format!("drop 0000000000 {}", ids[0]),
        )
        .await;
        assert!(!partial.is_error(), "{}", partial.text);
        assert!(
            partial.text.contains("no agreement has id 0000000000"),
            "{}",
            partial.text
        );
        assert!(!partial.text.contains(&ids[0]), "{}", partial.text);
        assert!(partial.text.contains(&ids[1]), "{}", partial.text);
    }
}
