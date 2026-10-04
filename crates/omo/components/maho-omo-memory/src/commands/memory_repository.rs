//! /memory-repository set|unset|status|push — push-only mirror configuration.
//! Port of `components/memory/commands/memory-repository.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use memory_core::sync::MirrorSync;

use super::args::{ParseCommandArgsOptions, parse_command_args_full};
use super::memfs_shared::require_existing_repo;
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, NotifyLevel,
};

const SUBCOMMANDS: [&str; 4] = ["set", "unset", "status", "push"];
const NOT_CONFIGURED: &str = "no memory repository configured; set one with /memory-repository set <url>";

pub async fn handle_memory_repository(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let parsed = parse_command_args_full(args, ParseCommandArgsOptions::default());
    let subcommand = parsed.positionals.first().cloned();
    let value = parsed.positionals.get(1).cloned();
    let Some(subcommand) = subcommand else {
        return respond(
            ctx,
            format!("usage: /memory-repository <{}> [url]", SUBCOMMANDS.join("|")),
            NotifyLevel::Error,
        );
    };

    let repo = match require_existing_repo(deps, &identity) {
        Ok(repo) => repo,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let mirror = match &deps.exec {
        Some(exec) => MirrorSync::with_exec(&repo, exec.as_ref()),
        None => MirrorSync::new(&repo),
    };

    match subcommand.as_str() {
        "set" => {
            let Some(value) = value else {
                return respond(ctx, "usage: /memory-repository set <url>", NotifyLevel::Error);
            };
            let result = match mirror.set(&value) {
                Ok(result) => result,
                Err(error) => return respond(ctx, error.to_string(), NotifyLevel::Error),
            };
            let status = mirror.status();
            let target = status.redacted_url.unwrap_or_else(|| "the configured mirror".to_owned());
            if result.pushed {
                respond(ctx, format!("memory repository set to {target}; initial push succeeded"), NotifyLevel::Info)
            } else {
                respond(
                    ctx,
                    format!(
                        "memory repository set to {target}; initial push failed: {}. Local commits are unaffected and every future commit retries the push.",
                        result.detail
                    ),
                    NotifyLevel::Warning,
                )
            }
        }
        "unset" => {
            if let Err(error) = mirror.unset() {
                return respond(ctx, error.to_string(), NotifyLevel::Error);
            }
            respond(ctx, "memory repository unset; commits stay local from now on", NotifyLevel::Info)
        }
        "status" => {
            let status = mirror.status();
            let Some(url) = status.url.clone() else {
                return respond(ctx, NOT_CONFIGURED, NotifyLevel::Info);
            };
            let _ = url;
            let mut lines = vec![
                format!("# Memory repository: {}", identity.identity),
                format!("Mirror: {}", status.redacted_url.clone().unwrap_or_default()),
                format!("Commits ahead of mirror: {}", status.ahead_count),
            ];
            if !status.last_log_lines.is_empty() {
                lines.push(String::new());
                lines.push("## Recent push log".to_owned());
                lines.extend(status.last_log_lines.clone());
            }
            respond(ctx, lines.join("\n"), NotifyLevel::Info)
        }
        "push" => {
            let status = mirror.status();
            let Some(redacted) = status.redacted_url.clone() else {
                return respond(ctx, NOT_CONFIGURED, NotifyLevel::Error);
            };
            let result = mirror.push_now();
            if result.pushed {
                respond(ctx, format!("pushed main to {redacted}"), NotifyLevel::Info)
            } else {
                respond(ctx, format!("push failed: {}", result.detail), NotifyLevel::Error)
            }
        }
        other => respond(
            ctx,
            format!(
                "unknown /memory-repository subcommand: {other}; expected one of {}",
                SUBCOMMANDS.join(", ")
            ),
            NotifyLevel::Error,
        ),
    }
}

pub fn register_memory_repository_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "memory-repository",
        Some("Configure the push-only mirror remote for the memory repository.".to_owned()),
        Some(format!("<{}> [url]", SUBCOMMANDS.join("|"))),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_memory_repository(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;
    use memory_core::sync::CONFIG_KEY;

    const CREDENTIALED_URL: &str = "https://user:s3cr3t-token@127.0.0.1:1/memory.git";

    fn seeds() -> Vec<memory_core::git::GitSeedFile> {
        vec![seed("system/persona.md", "---\ndescription: Persona\n---\nseeded persona\n")]
    }

    fn harness() -> (tempfile::TempDir, MemoryCommandIdentity, FakeDeps) {
        let (root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        (root, identity, fake)
    }

    #[tokio::test]
    async fn given_a_credentialed_url_when_set_runs_then_config_is_written_and_output_is_redacted() {
        let (_root, identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(
            &fake.deps,
            &context.ctx,
            &format!("set {CREDENTIALED_URL}"),
        )
        .await;

        let repo = open_repo(&identity);
        assert_eq!(repo.config_get(CONFIG_KEY).expect("config"), Some(CREDENTIALED_URL.to_owned()));
        assert!(response.text.contains("127.0.0.1"));
        assert!(!response.text.contains("s3cr3t-token"));
        assert!(context
            .ui
            .notifications()
            .iter()
            .all(|(message, _)| !message.contains("s3cr3t-token")));
    }

    #[tokio::test]
    async fn given_a_configured_mirror_when_status_runs_then_the_redacted_url_and_ahead_count_render() {
        let (_root, identity, fake) = harness();
        open_repo(&identity).config_set(CONFIG_KEY, CREDENTIALED_URL).expect("config set");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "status").await;

        assert!(response.text.contains("127.0.0.1"));
        assert!(!response.text.contains("s3cr3t-token"));
        assert!(response.text.contains("ahead"));
    }

    #[tokio::test]
    async fn given_no_configured_mirror_when_status_runs_then_it_reports_the_unconfigured_state() {
        let (_root, _identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "status").await;

        assert!(response.text.contains("no memory repository configured"));
    }

    #[tokio::test]
    async fn given_a_configured_mirror_when_unset_runs_then_the_config_key_is_removed() {
        let (_root, identity, fake) = harness();
        open_repo(&identity).config_set(CONFIG_KEY, CREDENTIALED_URL).expect("config set");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "unset").await;

        assert_eq!(open_repo(&identity).config_get(CONFIG_KEY).expect("config"), None);
        assert!(response.text.contains("unset"));
    }

    #[tokio::test]
    async fn given_no_configured_mirror_when_push_runs_then_an_actionable_error_is_returned() {
        let (_root, _identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "push").await;

        assert!(response.text.contains("no memory repository configured"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unreachable_mirror_when_push_runs_then_the_failure_is_reported_with_a_redacted_detail() {
        let (_root, identity, fake) = harness();
        open_repo(&identity).config_set(CONFIG_KEY, CREDENTIALED_URL).expect("config set");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "push").await;

        assert!(response.text.contains("push failed"));
        assert!(!response.text.contains("s3cr3t-token"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_set_without_a_url_when_invoked_then_a_usage_error_is_returned() {
        let (_root, _identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "set").await;

        assert!(response.text.contains("usage: /memory-repository set <url>"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unknown_subcommand_when_invoked_then_an_actionable_error_is_returned() {
        let (_root, _identity, fake) = harness();
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "frobnicate").await;

        assert!(response.text.contains("unknown /memory-repository subcommand"));
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_an_actionable_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory_repository(&fake.deps, &context.ctx, "status").await;

        assert!(response.text.contains("not bound"));
    }
}
