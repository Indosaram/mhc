//! The providers builtin extensions register instead of the catalog, for runtimes built outside a session
//! (`doctor`, `auth check`). omo `category-coverage.js` reads the engine's available models, which include
//! these registrations, so a diagnostic runtime registers them the way a session converts them.

use maho_core::model_runtime::ModelRuntime;

/// Registers anthropic-subscription (the extension's catalog half) and cursor-cli-oauth. With `store`, the
/// anthropic-subscription registration also carries the extension's account-aware OAuth, whose `check` counts the
/// stored claude CLI accounts the way a session's does. Returns the runtime and one `(provider, error)` per
/// registration that failed; the others still stand.
pub fn register_builtin_extension_providers(
    runtime: ModelRuntime,
    store: Option<std::sync::Arc<dyn maho_ai::auth::types::CredentialStore>>,
) -> (ModelRuntime, Vec<(String, String)>) {
    let mut failures = Vec::new();
    let subscription = maho_ext_anthropic_subscription::auth_lane::PROVIDER_ID;
    let actions = maho_core::agent_session_runtime::ExtensionModelRuntimeActions(
        std::sync::Mutex::new(runtime),
    );
    if let Err(error) = maho_ext_api::ExtensionProviderActions::register_provider(
        &actions,
        maho_ext_api::ProviderRegistration::Config {
            name: subscription.to_owned(),
            config: Box::new(maho_ext_api::ProviderConfig {
                oauth: store.map(|store| {
                    std::sync::Arc::new(
                        maho_ext_anthropic_subscription::oauth_login::AnthropicSubscriptionOAuth::native_with_resolver(
                            store,
                            std::sync::Arc::new(Default::default),
                            super::oauth_providers::claude_executable_resolver(),
                        ),
                    ) as std::sync::Arc<dyn maho_ai::auth::types::OAuthAuth>
                }),
                ..maho_ext_anthropic_subscription::extension::catalog_provider_config(
                    maho_ext_anthropic_subscription::long_context::long_context_available(),
                )
            }),
        },
        subscription,
    ) {
        failures.push((subscription.to_owned(), error.message));
    }
    let mut runtime = actions
        .0
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) = runtime.register_provider(
        "cursor-cli-oauth",
        maho_core::provider_composer::ProviderConfigInput::default(),
    ) {
        failures.push(("cursor-cli-oauth".to_owned(), error.to_string()));
    }
    (runtime, failures)
}
