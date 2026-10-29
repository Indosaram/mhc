//! `/remember <text>` — durable memory request turn.
//! Port of `components/memory/commands/remember.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::{ExtensionApi, UserMessageContent};

use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, NotifyLevel,
};

pub const REMEMBER_INSTRUCTION: &str =
    "[MEMORY REQUEST] Persist this as long-term memory if appropriate. Use your memory tools: \
choose the most appropriate memory file (create one if no relevant file exists), avoid \
duplicates, match the existing formatting of the file, then briefly confirm what you \
remembered and where you stored it.";

pub async fn handle_remember(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let text = args.trim();
    if text.is_empty() {
        return respond(ctx, "usage: /remember <text>", NotifyLevel::Error);
    }

    if let Some(wait) = &ctx.wait_for_idle {
        wait().await;
    }
    if let Err(error) = deps.actions.send_user_message(
        UserMessageContent::Text(format!("{REMEMBER_INSTRUCTION}\n\n{text}")),
        Default::default(),
    ) {
        return respond(ctx, error.to_string(), NotifyLevel::Error);
    }
    respond(ctx, format!("memory request sent for {}", identity.identity), NotifyLevel::Info)
}

pub fn register_remember_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "remember",
        Some("Persist text to long-term memory via a memory-tool turn.".to_owned()),
        Some("<text>".to_owned()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_remember(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    fn text_of(content: &UserMessageContent) -> String {
        match content {
            UserMessageContent::Text(text) => text.clone(),
            UserMessageContent::Blocks(_) => String::new(),
        }
    }

    #[tokio::test]
    async fn given_a_bound_identity_when_invoked_with_text_then_wait_for_idle_precedes_a_remember_turn() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let mut context = fake_command_context(FakeContextOptions::default());
        let order = fake.actions.order.clone();
        context.order = order.clone();
        context.ctx.wait_for_idle = Some(Arc::new(move || {
            let order = order.clone();
            Box::pin(async move {
                order.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("waitForIdle".to_owned());
            })
        }));

        let response = handle_remember(&fake.deps, &context.ctx, "I prefer tabs over spaces").await;

        assert_eq!(order_of(&context), vec!["waitForIdle".to_owned(), "sendUserMessage".to_owned()]);
        let messages = fake.actions.user_messages();
        assert_eq!(messages.len(), 1);
        let message = text_of(&messages[0]);
        assert!(message.contains("Persist this as long-term memory if appropriate"));
        assert!(message.contains("I prefer tabs over spaces"));
        assert!(response.text.contains("memory request sent"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_empty_text_when_invoked_then_a_usage_error_is_returned_and_nothing_is_sent() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_remember(&fake.deps, &context.ctx, "   ").await;

        assert!(response.text.contains("usage: /remember <text>"));
        assert!(fake.actions.user_messages().is_empty());
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_an_actionable_error_is_returned_and_nothing_is_sent() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_remember(&fake.deps, &context.ctx, "remember this").await;

        assert!(response.text.contains("not bound"));
        assert!(fake.actions.user_messages().is_empty());
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
