//! /memfs backup|restore|diff|tokens — thin git/filesystem wrappers.
//! Port of `components/memory/commands/memfs-extra.ts` at pin 77f3067f1.

use super::backup::{create_repo_backup, list_repo_backups, restore_repo_backup};
use super::memfs_shared::{MemfsSubcommandInput, require_existing_repo};
use super::repo::run_git;
use super::tokens::estimate_system_tokens;
use super::types::{BoxFuture, respond, NotifyLevel};

pub fn memfs_backup(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, super::types::CommandResponse> {
    Box::pin(async move {
        let repo = match require_existing_repo(input.deps, input.identity) {
            Ok(_repo) => _repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };
        let _ = repo;
        match create_repo_backup(input.deps, input.identity) {
            Ok(path) => respond(input.ctx, format!("backup written to {}", path.display()), NotifyLevel::Info),
            Err(error) => respond(input.ctx, error, NotifyLevel::Error),
        }
    })
}

pub fn memfs_restore(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, super::types::CommandResponse> {
    Box::pin(async move {
        let backups = list_repo_backups(input.identity);
        if backups.is_empty() {
            return respond(
                input.ctx,
                format!(
                    "no backups found for {} in {}",
                    input.identity.identity,
                    input.identity.identity_paths.root.display()
                ),
                NotifyLevel::Error,
            );
        }

        let requested = input.parsed.positionals.get(1).cloned();
        let name = match requested {
            None => {
                if !input.ctx.is_interactive() {
                    return respond(
                        input.ctx,
                        format!(
                            "specify a backup name: /memfs restore <name>; available: {}",
                            backups.join(", ")
                        ),
                        NotifyLevel::Error,
                    );
                }
                let selected = input.ctx.ui.select("Restore memory backup", &backups).await;
                match selected {
                    None => return respond(input.ctx, "restore cancelled; nothing changed", NotifyLevel::Info),
                    Some(selected) => selected,
                }
            }
            Some(name) => name,
        };

        if !backups.contains(&name) {
            return respond(
                input.ctx,
                format!("unknown backup: {name}; available: {}", backups.join(", ")),
                NotifyLevel::Error,
            );
        }

        match restore_repo_backup(input.deps, input.identity, &name) {
            Ok(result) => {
                let safety = result
                    .safety_backup
                    .map(|path| format!("; safety backup at {}", path.display()))
                    .unwrap_or_default();
                respond(
                    input.ctx,
                    format!(
                        "restored {} into {}{safety}",
                        result.restored,
                        input.identity.identity_paths.repo.display()
                    ),
                    NotifyLevel::Info,
                )
            }
            Err(error) => respond(input.ctx, error, NotifyLevel::Error),
        }
    })
}

pub fn memfs_diff(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, super::types::CommandResponse> {
    Box::pin(async move {
        let repo = match require_existing_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };

        let result = match run_git(input.deps, &repo.dir, &["diff"]) {
            Ok(result) => result,
            Err(error) => {
                return respond(input.ctx, format!("git diff failed: {error}"), NotifyLevel::Error);
            }
        };
        if result.code != 0 {
            let detail = if result.stderr.trim().is_empty() {
                format!("exit {}", result.code)
            } else {
                result.stderr.trim().to_owned()
            };
            return respond(input.ctx, format!("git diff failed: {detail}"), NotifyLevel::Error);
        }
        let diff = result.stdout.trim();
        if diff.is_empty() {
            return respond(input.ctx, "No changes.", NotifyLevel::Info);
        }
        respond(
            input.ctx,
            format!("# Memfs diff: {}\n\n{diff}", input.identity.identity),
            NotifyLevel::Warning,
        )
    })
}

pub fn memfs_tokens(input: &MemfsSubcommandInput<'_>) -> BoxFuture<'_, super::types::CommandResponse> {
    Box::pin(async move {
        let repo = match require_existing_repo(input.deps, input.identity) {
            Ok(repo) => repo,
            Err(error) => return respond(input.ctx, error, NotifyLevel::Error),
        };

        let estimate = estimate_system_tokens(&repo.dir);
        let mut lines = vec![
            format!(
                "# System prompt token estimate (bytes/4): {}",
                input.identity.identity
            ),
            format!(
                "Total: ~{} tokens ({} bytes)",
                estimate.total_tokens, estimate.total_bytes
            ),
        ];
        if !estimate.files.is_empty() {
            lines.push(String::new());
            lines.push("  tokens  path".to_owned());
            for file in &estimate.files {
                lines.push(format!("  {:>6}  {}", file.tokens, file.path));
            }
        }
        respond(input.ctx, lines.join("\n"), NotifyLevel::Info)
    })
}

#[cfg(test)]
mod tests {
    use super::super::memfs::handle_memfs;
    use crate::commands::test_support::*;
    use crate::commands::types::NotifyLevel;

    fn seeds() -> Vec<memory_core::git::GitSeedFile> {
        vec![seed("system/persona.md", "---\ndescription: Persona\n---\nseeded persona\n")]
    }

    fn identity_root(root: &std::path::Path) -> std::path::PathBuf {
        root.join("agents").join(TEST_IDENTITY)
    }

    fn backup_names(root: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(identity_root(root))
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
    async fn given_a_repository_when_backup_runs_then_a_sibling_memory_backup_dir_holds_a_copy() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "backup").await;

        let backups = backup_names(root.path());
        assert_eq!(backups.len(), 1);
        let name = &backups[0];
        assert!(name.len() == "memory-backup-YYYYMMDD-HHMMSS".len());
        assert!(response.text.contains(name.as_str()));
        let copied = std::fs::read_to_string(identity_root(root.path()).join(name).join("system").join("persona.md"))
            .expect("copied persona");
        assert!(copied.contains("seeded persona"));
    }

    #[tokio::test]
    async fn given_a_backup_already_exists_for_the_same_clock_when_backup_runs_again_then_the_existing_backup_is_not_clobbered() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        handle_memfs(&fake.deps, &context.ctx, "backup").await;
        handle_memfs(&fake.deps, &context.ctx, "backup").await;

        let backups = backup_names(root.path());
        assert_eq!(backups.len(), 2);
        let unique: std::collections::BTreeSet<String> = backups.into_iter().collect();
        assert_eq!(unique.len(), 2);
    }

    #[tokio::test]
    async fn given_a_named_backup_when_restore_runs_then_repo_content_is_replaced_and_a_safety_backup_is_kept() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        handle_memfs(&fake.deps, &context.ctx, "backup").await;
        let backup_name = backup_names(root.path())[0].clone();
        std::fs::write(
            identity.identity_paths.repo.join("system").join("persona.md"),
            "---\ndescription: Persona\n---\nlocal edit\n",
        )
        .expect("local edit");

        let response = handle_memfs(&fake.deps, &context.ctx, &format!("restore {backup_name}")).await;

        let restored = std::fs::read_to_string(identity.identity_paths.repo.join("system").join("persona.md"))
            .expect("restored");
        assert!(restored.contains("seeded persona"));
        assert!(response.text.contains("restored"));
        assert_eq!(backup_names(root.path()).len(), 2);
    }

    #[tokio::test]
    async fn given_no_backups_when_restore_runs_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "restore").await;

        assert!(response.text.contains("no backups found"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_backups_and_an_interactive_session_when_restore_runs_without_a_name_then_the_user_selects_one() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions { has_ui: Some(true), ..Default::default() });
        handle_memfs(&fake.deps, &context.ctx, "backup").await;
        let backup_name = backup_names(root.path())[0].clone();
        context.ui.set_select_result(Some(backup_name.clone()));
        std::fs::write(
            identity.identity_paths.repo.join("system").join("persona.md"),
            "---\ndescription: Persona\n---\nlocal edit\n",
        )
        .expect("local edit");

        let response = handle_memfs(&fake.deps, &context.ctx, "restore").await;

        let selects = context.ui.selects.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        assert_eq!(selects.len(), 1);
        assert!(selects[0].1.contains(&backup_name));
        assert!(response.text.contains("restored"));
        assert!(std::fs::read_to_string(identity.identity_paths.repo.join("system").join("persona.md"))
            .expect("restored")
            .contains("seeded persona"));
    }

    #[tokio::test]
    async fn given_backups_and_a_noninteractive_session_when_restore_runs_without_a_name_then_available_backups_are_listed() {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let interactive = fake_command_context(FakeContextOptions { has_ui: Some(true), ..Default::default() });
        handle_memfs(&fake.deps, &interactive.ctx, "backup").await;
        let backup_name = backup_names(root.path())[0].clone();
        let headless = fake_command_context(FakeContextOptions { has_ui: Some(false), ..Default::default() });

        let response = handle_memfs(&fake.deps, &headless.ctx, "restore").await;

        assert!(response.text.contains("specify a backup name"));
        assert!(response.text.contains(&backup_name));
        assert_eq!(headless.ui.last_level(), Some(NotifyLevel::Error));
        assert!(interactive.ui.selects.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
    }

    #[tokio::test]
    async fn given_an_unknown_backup_name_when_restore_runs_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());
        handle_memfs(&fake.deps, &context.ctx, "backup").await;

        let response = handle_memfs(&fake.deps, &context.ctx, "restore memory-backup-19700101-000000").await;

        assert!(response.text.contains("unknown backup"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_a_clean_tree_when_diff_runs_then_no_changes_are_reported_at_info_level() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "diff").await;

        assert!(response.text.contains("No changes."));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_tracked_modifications_when_diff_runs_then_the_diff_is_returned_as_a_warning() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        std::fs::write(
            identity.identity_paths.repo.join("system").join("persona.md"),
            "---\ndescription: Persona\n---\nedited body\n",
        )
        .expect("edit");
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "diff").await;

        assert!(response.text.contains("system/persona.md"));
        assert!(response.text.contains("edited body"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Warning));
    }

    #[tokio::test]
    async fn given_system_files_when_tokens_runs_then_a_bytes_four_estimate_and_per_file_rows_render() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let persona = std::fs::read_to_string(identity.identity_paths.repo.join("system").join("persona.md"))
            .expect("persona");
        let expected = persona.len().div_ceil(4);
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "tokens").await;

        assert!(response.text.contains(&format!("Total: ~{expected} tokens")));
        assert!(response.text.contains("system/persona.md"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_no_repository_when_tokens_runs_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memfs(&fake.deps, &context.ctx, "tokens").await;

        assert!(response.text.contains("no memory repository"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
