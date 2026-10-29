//! `/recompile` — bust the compiled memory-block cache so the next agent run
//! recompiles from HEAD.
//! Port of `components/memory/commands/recompile.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;

use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, NotifyLevel,
};

pub async fn handle_recompile(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    _args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    (deps.bust_prompt_cache)();
    respond(
        ctx,
        format!(
            "memory prompt cache cleared for {}; the next agent run recompiles from HEAD (the current run keeps its prompt)",
            identity.identity
        ),
        NotifyLevel::Info,
    )
}

pub fn register_recompile_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "recompile",
        Some("Recompile the memory block into the system prompt on the next agent run.".to_owned()),
        Some(String::new()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_recompile(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    #[tokio::test]
    async fn given_a_bound_identity_when_invoked_then_the_prompt_cache_is_busted_for_the_next_run() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_recompile(&fake.deps, &context.ctx, "").await;

        assert_eq!(busts(&fake).len(), 1);
        assert!(response.text.contains("next agent run"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_nothing_is_busted_and_an_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_recompile(&fake.deps, &context.ctx, "").await;

        assert!(busts(&fake).is_empty());
        assert!(response.text.contains("not bound"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
