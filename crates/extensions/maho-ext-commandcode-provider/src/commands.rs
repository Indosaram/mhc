use crate::contract::ProviderEnv;
use crate::quota_command::{QUOTA_COMMAND, QUOTA_MISSING_KEY, REFRESH_COMMAND, STATUS_COMMAND};
use maho_ext_api::{ExtensionApi, NotificationType};
use std::sync::Arc;

pub fn pick_api_key(registry: Option<String>, configured: Option<String>) -> Option<String> {
    registry
        .filter(|key| !crate::api_key::is_placeholder(key) && !key.trim().is_empty())
        .or_else(|| {
            configured.filter(|key| !crate::api_key::is_placeholder(key) && !key.trim().is_empty())
        })
}

pub fn register(api: &mut ExtensionApi, env: ProviderEnv) {
    let quota_env = env.clone();
    api.register_command(
        QUOTA_COMMAND,
        Some("Show Command Code account usage and quota".to_owned()),
        None,
        Arc::new(move |_args, context| {
            let env = quota_env.clone();
            Box::pin(async move {
                context.wait_for_idle().await;
                let registry = context
                    .model_registry
                    .get_api_key_for_provider(crate::contract::PROVIDER_ID)
                    .await
                    .ok()
                    .flatten();
                let configured = crate::api_key::get_configured_api_key(&env);
                let Some(api_key) = pick_api_key(registry, configured) else {
                    context
                        .ui
                        .notify(QUOTA_MISSING_KEY, NotificationType::Warning);
                    return Ok(());
                };
                let snapshot =
                    crate::quota::fetch_quota(&env.legacy_api_base(), Some(&api_key)).await;
                if let Some(warning) = crate::quota_command::quota_unavailable_message(&snapshot) {
                    context.ui.notify(warning, NotificationType::Error);
                } else {
                    context.ui.notify(
                        &crate::quota_format::format_quota(&snapshot),
                        NotificationType::Info,
                    );
                }
                Ok(())
            })
        }),
    );
    let status_env = env.clone();
    api.register_command(
        STATUS_COMMAND,
        Some("Show the Command Code model catalog status".to_owned()),
        None,
        Arc::new(move |_args, context| {
            let env = status_env.clone();
            Box::pin(async move {
                let status = crate::runtime::format_status(&crate::runtime::status(&env));
                context.ui.notify(&status, NotificationType::Info);
                Ok(())
            })
        }),
    );
    api.register_command(
        REFRESH_COMMAND,
        Some("Refresh the Command Code model catalog".to_owned()),
        None,
        Arc::new(move |_args, context| {
            let env = env.clone();
            Box::pin(async move {
                let result = crate::runtime::refresh(&env).await;
                let message = crate::runtime::refresh_result_warning(&result)
                    .unwrap_or_else(|| "Command Code model catalog refreshed".to_owned());
                context.ui.notify(&message, NotificationType::Info);
                Ok(())
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_key_wins_over_the_configured_one() {
        assert_eq!(
            pick_api_key(Some("registry".into()), Some("configured".into())),
            Some("registry".into())
        );
        assert_eq!(
            pick_api_key(None, Some("configured".into())),
            Some("configured".into())
        );
    }

    #[test]
    fn registers_the_three_command_code_commands() {
        use maho_ext_api::types::*;
        let mut api = ExtensionApi::new(
            LoadedExtension::new("commandcode", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        register(&mut api, ProviderEnv::from_process());
        let names: Vec<&str> = api
            .registered
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![QUOTA_COMMAND, STATUS_COMMAND, REFRESH_COMMAND],
            "every TS-registered command must exist natively"
        );
        for command in &api.registered.commands {
            assert!(
                command.description.is_some(),
                "{} must carry the description the TS command declares",
                command.name
            );
        }
    }

    #[test]
    fn placeholder_keys_are_not_api_keys() {
        assert_eq!(
            pick_api_key(Some("$COMMAND_CODE_API_KEY".into()), None),
            None
        );
        assert_eq!(pick_api_key(Some("  ".into()), None), None);
    }
}
