//! `/init` — initialize the memory repository and send a standardized init turn.
//! Port of `components/memory/commands/init.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::{ExtensionApi, UserMessageContent};

use super::repo::{has_git_repo, open_repo, short_sha};
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, NotifyLevel,
};

fn init_instruction(repo_path: &str) -> String {
    [
        "[MEMORY INITIALIZATION]",
        &format!(
            "The user invoked /init. Your memory repository was just initialized at {repo_path} and is projected on the local filesystem. Inspect it before writing."
        ),
        "",
        "Create your initial memory now:",
        "- system/persona.md — who you are: identity, communication style, durable behavioral rules. This file compiles into every future system prompt; keep it curated.",
        "- system/human.md — what you know about the user: goals, preferences, collaboration style.",
        "- Additional system/ index files when useful — compact summaries with [[path]] discovery links into external memory.",
        "",
        "Every memory file uses this format:",
        "---",
        "description: <single-line purpose>",
        "---",
        "<body>",
        "",
        "Store durable, generalizable knowledge, not transient session state. Do not overwrite existing files; extend them.",
    ]
    .join("\n")
}

pub async fn handle_init(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    _args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let repo = match open_repo(deps, &identity) {
        Ok(repo) => repo,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let head = if has_git_repo(&identity) {
        repo.head().ok().flatten()
    } else {
        None
    };
    if let Some(head) = head {
        return respond(
            ctx,
            format!(
                "memory already initialized for {} (HEAD {}); use /memory to view or /doctor to audit",
                identity.identity,
                short_sha(&head)
            ),
            NotifyLevel::Error,
        );
    }

    if let Err(error) = repo.init(None) {
        return respond(ctx, error.to_string(), NotifyLevel::Error);
    }
    if let Some(wait) = &ctx.wait_for_idle {
        wait().await;
    }
    if let Err(error) = deps.actions.send_user_message(
        UserMessageContent::Text(init_instruction(&identity.identity_paths.repo.display().to_string())),
        Default::default(),
    ) {
        return respond(ctx, error.to_string(), NotifyLevel::Error);
    }
    respond(
        ctx,
        format!(
            "initialized memory repository at {}; initialization turn sent",
            identity.identity_paths.repo.display()
        ),
        NotifyLevel::Info,
    )
}

pub fn register_init_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "init",
        Some("Initialize the memory repository and instruct the agent to create initial memory.".to_owned()),
        Some(String::new()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_init(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    #[tokio::test]
    async fn given_no_repository_when_invoked_then_the_repo_is_initialized_and_an_instruction_turn_follows_idle() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_init(&fake.deps, &context.ctx, "").await;

        let repo = crate::commands::repo::open_repo(&fake.deps, &identity).expect("repo");
        assert!(repo.head().expect("head").is_some());
        assert_eq!(order_of(&context), vec!["waitForIdle".to_owned(), "sendUserMessage".to_owned()]);
        let messages = fake.actions.user_messages();
        assert_eq!(messages.len(), 1);
        let message = match &messages[0] {
            UserMessageContent::Text(text) => text.clone(),
            UserMessageContent::Blocks(_) => String::new(),
        };
        assert!(message.contains("[MEMORY INITIALIZATION]"));
        assert!(message.contains("system/persona.md"));
        assert!(message.contains("description:"));
        assert!(response.text.contains("initialized"));
    }

    #[tokio::test]
    async fn given_an_existing_repository_when_invoked_then_it_refuses_without_overwriting_and_sends_nothing() {
        let (_root, identity) = temp_identity();
        let repo = seeded_repo(
            &identity,
            vec![seed("system/persona.md", "---\ndescription: persona\n---\nkeep me\n")],
        );
        let head_before = repo.head().expect("head");
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_init(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("already initialized"));
        assert_eq!(repo.head().expect("head"), head_before);
        assert!(
            repo.show("HEAD", "system/persona.md")
                .expect("show")
                .contains("keep me")
        );
        assert!(fake.actions.user_messages().is_empty());
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_an_actionable_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_init(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("not bound"));
        assert!(fake.actions.user_messages().is_empty());
    }
}
