//! /memfs <status|init|sync|repair|reset|backup|restore|diff|tokens>
//! Port of `components/memory/commands/memfs.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use memory_core::{
    git::GitMemoryRepo,
    memfs::install_hooks,
    sync::MirrorSync,
};

use super::args::{ParseCommandArgsOptions, parse_command_args_full};
use super::backup::create_repo_backup;
use super::memfs_extra::{memfs_backup, memfs_diff, memfs_restore, memfs_tokens};
use super::memfs_shared::{MemfsSubcommand, MemfsSubcommandInput, require_existing_repo};
use super::repo::{has_git_repo, open_repo, short_sha};
use super::skill_frontmatter::{
    format_skill_name_frontmatter_repair_report, repair_missing_skill_name_frontmatter,
};
use super::types::{
    BoxFuture, CommandContext, CommandResponse, MemoryCommandDeps, NotifyLevel,
    command_context_from, finish, identity_for_init, require_identity, respond,
};

const SUBCOMMANDS: [&str; 9] = [
    "status", "init", "sync", "repair", "reset", "backup", "restore", "diff", "tokens",
];

const SKILLS_USAGE_FILENAME: &str = "skills-usage.json";

fn mirror_for<'a>(
    repo: &'a GitMemoryRepo,
    deps: &'a MemoryCommandDeps,
) -> MirrorSync<'a> {
    match &deps.exec {
        Some(exec) => MirrorSync::with_exec(repo, exec.as_ref()),
        None => MirrorSync::new(repo),
    }
}

fn memfs_status(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, CommandResponse> {
    Box::pin(async move {
        let repo = match require_existing_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };

        let head = repo.head().ok().flatten();
        let porcelain: Vec<String> = repo
            .status(&[] as &[&str])
            .unwrap_or_default()
            .split('\n')
            .map(|line| line.trim_end_matches('\r').to_owned())
            .filter(|line| !line.trim().is_empty())
            .collect();
        let mirror = mirror_for(&repo, input.deps).status();
        let dirty = !porcelain.is_empty();

        let head_text = match &head {
            Some(head) => short_sha(head),
            None => "(no commits)".to_owned(),
        };
        let mirror_text = match (&mirror.redacted_url, &mirror.url) {
            (Some(redacted), Some(_)) => format!("{redacted} ({} ahead)", mirror.ahead_count),
            (Some(redacted), None) => redacted.clone(),
            (None, _) => "not configured".to_owned(),
        };
        let mut lines = vec![
            format!("# Memfs status: {}", input.identity.identity),
            format!("Repository: {}", input.identity.identity_paths.repo.display()),
            format!("HEAD: {head_text}"),
            format!(
                "Working tree: {}",
                if dirty {
                    format!(
                        "dirty — {} entr{}",
                        porcelain.len(),
                        if porcelain.len() == 1 { "y" } else { "ies" }
                    )
                } else {
                    "clean".to_owned()
                }
            ),
            format!("Mirror: {mirror_text}"),
        ];
        if dirty {
            lines.push(String::new());
            lines.extend(porcelain);
            lines.push(String::new());
            lines.push(
                "uncommitted changes present; inspect with /memfs diff or commit through the memory tools"
                    .to_owned(),
            );
        }
        if let Some(block) = render_skills_usage(&input.identity.identity_paths.runtime) {
            lines.push(block);
        }
        respond(
            input.ctx,
            lines.join("\n"),
            if dirty { NotifyLevel::Warning } else { NotifyLevel::Info },
        )
    })
}

fn memfs_init(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, CommandResponse> {
    Box::pin(async move {
        if has_git_repo(input.identity) {
            let head = open_repo(input.deps, input.identity).ok().and_then(|repo| repo.head().ok().flatten());
            let suffix = head
                .map(|head| format!(" (HEAD {})", short_sha(&head)))
                .unwrap_or_default();
            return respond(
                input.ctx,
                format!(
                    "memory repository already initialized at {}{suffix}",
                    input.identity.identity_paths.repo.display()
                ),
                NotifyLevel::Info,
            );
        }
        let repo = match open_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };
        match repo.init(None) {
            Ok(sha) => respond(
                input.ctx,
                format!(
                    "initialized memory repository at {} (HEAD {})",
                    input.identity.identity_paths.repo.display(),
                    short_sha(&sha)
                ),
                NotifyLevel::Info,
            ),
            Err(error) => respond(input.ctx, error.to_string(), NotifyLevel::Error),
        }
    })
}

fn memfs_sync(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, CommandResponse> {
    Box::pin(async move {
        let repo = match require_existing_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };

        let mirror = mirror_for(&repo, input.deps);
        let status = mirror.status();
        let Some(redacted) = status.redacted_url.clone() else {
            return respond(
                input.ctx,
                "no memory repository mirror configured; set one with /memory-repository set <url>",
                NotifyLevel::Error,
            );
        };
        let result = mirror.push_now();
        if !result.pushed {
            return respond(
                input.ctx,
                format!("push to {redacted} failed: {}", result.detail),
                NotifyLevel::Error,
            );
        }
        respond(input.ctx, format!("pushed main to {redacted}"), NotifyLevel::Info)
    })
}

fn memfs_repair(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, CommandResponse> {
    Box::pin(async move {
        let repo = match require_existing_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };

        let hooks = install_hooks(&repo.dir).unwrap_or_default();
        let repaired = repair_missing_skill_name_frontmatter(Some(&repo.dir));
        let report = format_skill_name_frontmatter_repair_report(&repaired);
        let mut lines = vec![
            format!("# Memfs repair: {}", input.identity.identity),
            format!(
                "- reinstalled {} git hook{}",
                hooks.len(),
                if hooks.len() == 1 { "" } else { "s" }
            ),
            format!(
                "- scanned {} skill file{}",
                repaired.scanned,
                if repaired.scanned == 1 { "" } else { "s" }
            ),
        ];
        if !report.is_empty() {
            lines.extend(report.split('\n').filter(|line| !line.is_empty()).map(|line| format!("- {line}")));
        }
        respond(input.ctx, lines.join("\n"), NotifyLevel::Info)
    })
}

fn memfs_reset(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, CommandResponse> {
    Box::pin(async move {
        let existing = match require_existing_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };
        let _ = existing;

        if input.ctx.is_interactive() {
            let confirmed = input
                .ctx
                .ui
                .confirm(
                    "Reset memory repository",
                    &format!(
                        "This deletes {} and starts an empty history. A backup is written first.",
                        input.identity.identity_paths.repo.display()
                    ),
                )
                .await;
            if !confirmed {
                return respond(input.ctx, "reset aborted; nothing changed", NotifyLevel::Info);
            }
        } else if !input.parsed.flag_is_true("force") {
            return respond(
                input.ctx,
                "reset requires --force in a noninteractive session (no confirmation UI available): /memfs reset --force",
                NotifyLevel::Error,
            );
        }

        let backup = match create_repo_backup(input.deps, input.identity) {
            Ok(backup) => backup,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };
        if let Err(error) = std::fs::remove_dir_all(&input.identity.identity_paths.repo)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return respond(input.ctx, error.to_string(), NotifyLevel::Error);
        }
        let repo = match open_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };
        match repo.init(None) {
            Ok(sha) => respond(
                input.ctx,
                format!(
                    "reset complete; backup at {}; new HEAD {}",
                    backup.display(),
                    short_sha(&sha)
                ),
                NotifyLevel::Info,
            ),
            Err(error) => respond(input.ctx, error.to_string(), NotifyLevel::Error),
        }
    })
}

fn render_skills_usage(runtime_dir: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(runtime_dir.join(SKILLS_USAGE_FILENAME)).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    let entries = parsed.as_object()?;
    if entries.is_empty() {
        return None;
    }
    let mut lines = vec![String::new(), "## Skills usage".to_owned()];
    for (skill_id, value) in entries {
        let Some(entry) = value.as_object() else {
            continue;
        };
        let count = entry.get("count").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let last_used_at = entry
            .get("lastUsedAt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        lines.push(format!(
            "  {skill_id}: {count} read{}, last {last_used_at}",
            if count == 1 { "" } else { "s" }
        ));
    }
    if lines.len() > 2 {
        Some(lines.join("\n"))
    } else {
        None
    }
}

fn handler_for(subcommand: &str) -> Option<MemfsSubcommand> {
    match subcommand {
        "status" => Some(memfs_status),
        "init" => Some(memfs_init),
        "sync" => Some(memfs_sync),
        "repair" => Some(memfs_repair),
        "reset" => Some(memfs_reset),
        "backup" => Some(memfs_backup),
        "restore" => Some(memfs_restore),
        "diff" => Some(memfs_diff),
        "tokens" => Some(memfs_tokens),
        _ => None,
    }
}

pub async fn handle_memfs(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let parsed = parse_command_args_full(args, ParseCommandArgsOptions { booleans: &["force"] });
    let Some(subcommand) = parsed.positionals.first().cloned() else {
        return respond(
            ctx,
            format!("usage: /memfs <{}>", SUBCOMMANDS.join("|")),
            NotifyLevel::Error,
        );
    };
    let Some(handler) = handler_for(&subcommand) else {
        return respond(
            ctx,
            format!(
                "unknown /memfs subcommand: {subcommand}; expected one of {}",
                SUBCOMMANDS.join(", ")
            ),
            NotifyLevel::Error,
        );
    };

    let identity = if subcommand == "init" {
        identity_for_init(deps, ctx)
    } else {
        match require_identity(deps, ctx) {
            Ok(identity) => Some(identity),
            Err(error) => return respond(ctx, error, NotifyLevel::Error),
        }
    };
    let Some(identity) = identity else {
        return respond(
            ctx,
            "no memory identity resolved; check the memory config for this project",
            NotifyLevel::Error,
        );
    };

    let input = MemfsSubcommandInput { deps, ctx, parsed: &parsed, identity: &identity };
    handler(&input).await
}

pub fn register_memfs_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "memfs",
        Some("Inspect and maintain the memory filesystem repository.".to_owned()),
        Some(format!("<{}>", SUBCOMMANDS.join("|"))),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_memfs(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    fn seeds() -> Vec<memory_core::git::GitSeedFile> {
        vec![seed("system/persona.md", "---\ndescription: Persona\n---\nseeded persona\n")]
    }

    fn backup_names(root: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root.join("agents").join(TEST_IDENTITY))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .filter(|name| name.starts_with("memory-backup-"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[tokio::test]
    async fn given_a_clean_repo_when_status_runs_then_head_and_a_clean_tree_are_reported_at_info_level() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "status").await;

        assert!(response.text.contains("HEAD:"));
        assert!(response.text.contains("Working tree: clean"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
        let _ = root;
    }

    #[tokio::test]
    async fn given_uncommitted_changes_when_status_runs_then_the_dirty_state_is_reported_as_a_warning() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        std::fs::write(
            identity.identity_paths.repo.join("system").join("persona.md"),
            "---\ndescription: Persona\n---\nedited\n",
        )
        .expect("dirty");
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "status").await;

        assert!(response.text.contains("Working tree: dirty"));
        assert!(response.text.contains("uncommitted changes"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Warning));
    }

    #[tokio::test]
    async fn given_no_repository_when_status_runs_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "status").await;

        assert!(response.text.contains("no memory repository"));
        assert!(response.text.contains("/memfs init"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_no_repository_when_init_runs_then_the_repository_is_created() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "init").await;

        assert!(response.text.contains("initialized memory repository"));
        assert!(identity.identity_paths.repo.join(".git").exists());
    }

    #[tokio::test]
    async fn given_an_existing_repository_when_init_runs_then_it_reports_idempotently_without_error() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "init").await;

        assert!(response.text.contains("already initialized"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_an_unbound_session_with_a_config_resolved_identity_when_init_runs_then_it_still_initializes() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(
            None,
            FakeDepsOverrides { resolve_identity: Some(Some(identity.clone())), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "init").await;

        assert!(response.text.contains("initialized memory repository"));
        assert!(identity.identity_paths.repo.join(".git").exists());
    }

    #[tokio::test]
    async fn given_no_configured_mirror_when_sync_runs_then_an_actionable_error_names_memory_repository() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "sync").await;

        assert!(response.text.contains("no memory repository mirror configured"));
        assert!(response.text.contains("/memory-repository set"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_a_skill_missing_name_frontmatter_when_repair_runs_then_hooks_are_reinstalled_and_the_skill_is_repaired() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        std::fs::create_dir_all(identity.identity_paths.repo.join("skills").join("commit")).expect("dir");
        std::fs::write(
            identity.identity_paths.repo.join("skills").join("commit").join("SKILL.md"),
            "---\ndescription: x\n---\n",
        )
        .expect("skill");
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "repair").await;

        assert!(response.text.contains("skills/commit/SKILL.md"));
        assert!(response.text.contains("hooks"));
        assert!(identity.identity_paths.repo.join(".git").join("hooks").join("pre-commit").exists());
    }

    #[tokio::test]
    async fn given_interactive_confirmation_when_reset_runs_then_a_backup_is_taken_first_and_the_repo_is_reinitialized() {
        let (root, identity) = temp_identity();
        let repo = seeded_repo(&identity, seeds());
        let head_before = repo.head().expect("head");
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions { has_ui: Some(true), ..Default::default() });

        let response = handle_memfs(&fake.deps, &context.ctx, "reset").await;

        assert_eq!(context.ui.confirms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1);
        let backups = backup_names(root.path());
        assert_eq!(backups.len(), 1);
        assert!(root
            .path()
            .join("agents")
            .join(TEST_IDENTITY)
            .join(&backups[0])
            .join("system")
            .join("persona.md")
            .exists());
        assert!(response.text.contains("backup"));
        assert!(!identity.identity_paths.repo.join("system").join("persona.md").exists());
        assert_ne!(repo.head().expect("head"), head_before);
    }

    #[tokio::test]
    async fn given_a_declined_confirmation_when_reset_runs_then_nothing_is_destroyed_and_no_backup_is_written() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions { has_ui: Some(true), ..Default::default() });
        context.ui.set_confirm_result(false);

        let response = handle_memfs(&fake.deps, &context.ctx, "reset").await;

        assert!(response.text.contains("aborted"));
        assert!(backup_names(root.path()).is_empty());
        assert!(identity.identity_paths.repo.join("system").join("persona.md").exists());
    }

    #[tokio::test]
    async fn given_a_noninteractive_session_without_force_when_reset_runs_then_it_refuses_and_preserves_the_repo() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions { has_ui: Some(false), ..Default::default() });

        let response = handle_memfs(&fake.deps, &context.ctx, "reset").await;

        assert!(response.text.contains("--force"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
        assert!(backup_names(root.path()).is_empty());
        assert!(identity.identity_paths.repo.join("system").join("persona.md").exists());
    }

    #[tokio::test]
    async fn given_a_noninteractive_session_with_force_when_reset_runs_then_the_repo_is_backed_up_and_reset() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions { has_ui: Some(false), ..Default::default() });

        let response = handle_memfs(&fake.deps, &context.ctx, "reset --force").await;

        assert!(response.text.contains("reset complete"));
        assert_eq!(backup_names(root.path()).len(), 1);
        assert!(!identity.identity_paths.repo.join("system").join("persona.md").exists());
    }

    #[tokio::test]
    async fn given_no_subcommand_when_invoked_then_usage_lists_every_subcommand() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("usage: /memfs"));
        for sub in SUBCOMMANDS {
            assert!(response.text.contains(sub));
        }
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unknown_subcommand_when_invoked_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "frobnicate").await;

        assert!(response.text.contains("unknown /memfs subcommand: frobnicate"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
