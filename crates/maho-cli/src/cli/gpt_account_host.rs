use maho_ai::auth::types::{AccountLoginReceipt, AuthInteraction, ProviderAuthInteraction};
use maho_ext_api::ExtensionFailure;
use std::sync::Arc;

pub async fn login_account(
    interaction: &Arc<maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction>,
    runtime: &maho_core::model_runtime::ModelRuntime,
) -> Result<Option<AccountLoginReceipt>, ExtensionFailure> {
    let oauth = maho_ai::auth::oauth::load::load_chatgpt_subscription_oauth()
        .await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
    let signal = interaction.signal().unwrap_or_else(|| maho_ai::utils::abort::AbortController::new().signal());
    let credential = oauth.login(&ProviderAuthInteraction::new(signal.clone(), interaction.clone()))
        .await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
    let receipt = runtime.credentials.append_oauth_account(
        maho_ext_builtin_loose::gpt_account::PROVIDER_ID,
        credential,
        Some(signal),
    ).await.map_err(ExtensionFailure::new)?;
    if let Some(receipt) = &receipt {
        maho_ai::auth::types::AuthInteraction::on_account_committed(interaction.as_ref(), receipt.clone());
    }
    let _ = runtime.models().refresh(maho_ai::models::ModelsRefreshOptions {
        providers: Some(vec![maho_ext_builtin_loose::gpt_account::PROVIDER_ID.to_owned()]),
        signal: interaction.signal(),
        ..Default::default()
    }).await;
    maho_core::provider_account_events::emit_provider_accounts_changed(maho_ext_builtin_loose::gpt_account::PROVIDER_ID);
    Ok(receipt)
}
