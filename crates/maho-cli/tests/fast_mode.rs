use maho_core::{model_runtime::{CreateModelRuntimeOptions, ModelRuntime},sdk::{CreateAgentSessionOptions,NoToolsMode,create_agent_session},session_manager::SessionManager,settings_manager::SettingsManager};
use maho_ext_builtin_loose::service_tier::{CODEX_RESPONSES_API, ServiceTierExtension};
use maho_ext_api::{Extension, ServiceTier};
use std::sync::Arc;

fn catalog_models() -> Vec<maho_ai::types::Model> {
    [
        ("codex", None, Some("wire-codex")),
        ("codex-fast", Some("priority"), Some("wire-codex")),
        ("codex-mismatch-fast", Some("priority"), Some("wire-other")),
    ].into_iter().map(|(id, tier, upstream)| {
        serde_json::from_value(serde_json::json!({
            "id":id,"name":id,"api":CODEX_RESPONSES_API,"provider":"fixture",
            "baseUrl":"https://example.invalid","reasoning":true,"input":["text"],
            "contextWindow":128000,"maxTokens":4096,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},
            "serviceTier":tier,"upstreamModelId":upstream
        })).expect("model")
    }).collect()
}


async fn create_fast_session(
    dir: &std::path::Path,
    model_id: &str,
    service_tier: Option<ServiceTier>,
    catalog: Vec<maho_ai::types::Model>,
) -> maho_core::agent_session::AgentSession {
    let cwd=dir.to_string_lossy().into_owned();
    let agent_dir=dir.join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let mut runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path:Some(dir.join("models.json")),auth_path:Some(dir.join("auth.json")),providers:Some(Vec::new()),..Default::default()
    }).expect("valid model runtime");
    runtime.register_provider("fixture",maho_core::provider_composer::ProviderConfigInput {
        config:maho_core::model_config_schema::ModelsJsonProvider {
            api:Some(CODEX_RESPONSES_API.into()),base_url:Some("https://example.invalid".into()),api_key:Some("synthetic-fast-mode".into()),
            models:Some(catalog.into_iter().map(|model|maho_core::model_config_schema::ModelsJsonModel {
                id:model.id,name:Some(model.name),api:Some(model.api),base_url:Some(model.base_url),upstream_model_id:model.upstream_model_id,
                service_tier:model.service_tier,context_window:Some(model.context_window as f64),max_tokens:Some(model.max_tokens as f64),..Default::default()
            }).collect()),..Default::default()
        },..Default::default()
    }).expect("fixture provider");
    let model=runtime.models().get_model("fixture",model_id).expect("selected catalog model");
    let host=Arc::new(maho_cli::cli::default_extensions::test_service_tier_host());
    let extension=maho_ext_host::loader::NativeAsyncExtensionFactory {
        path:"<builtin:service-tier>".into(),source_info:Default::default(),
        factory:Arc::new(move|api|{
            let extension=ServiceTierExtension{host:host.clone()};
            Box::pin(async move{extension.register(api);Ok(())})
        }),
    };
    let settings=SettingsManager::create(&cwd,&agent_dir.to_string_lossy(),&dir.to_string_lossy(),false);
    let scoped_models=service_tier.map(|tier|vec![maho_core::agent_session::SessionModelEntry {
        model:model.clone(),thinking_level:None,thinking_selection:None,service_tier:Some(tier),
    }]).unwrap_or_default();
    create_agent_session(CreateAgentSessionOptions {
        cwd:Some(cwd.clone()),agent_dir:Some(agent_dir.to_string_lossy().into_owned()),model_runtime:Some(runtime),model:Some(model),
        session_manager:Some(SessionManager::in_memory(&cwd,None,None)),settings_manager:Some(settings),extension_factories:vec![extension],scoped_models,
        no_tools:Some(NoToolsMode::All),auto_title_sessions:Some(false),..Default::default()
    }).await.expect("real fast-mode session").session
}


async fn create_fast_session_with_sibling_provider(dir:&std::path::Path)->maho_core::agent_session::AgentSession{
    let cwd=dir.to_string_lossy().into_owned();let agent_dir=dir.join("agent");std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let mut runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions{models_path:Some(dir.join("models.json")),auth_path:Some(dir.join("auth.json")),providers:Some(Vec::new()),..Default::default()}).expect("valid model runtime");
    for (provider,id,tier,upstream) in [("fixture","codex",None,None),("other-fixture","codex-fast",Some(maho_ai::types::ServiceTierPreference::Priority),Some("wire-codex"))]{
        runtime.register_provider(provider,maho_core::provider_composer::ProviderConfigInput{config:maho_core::model_config_schema::ModelsJsonProvider{
            api:Some(CODEX_RESPONSES_API.into()),base_url:Some("https://example.invalid".into()),api_key:Some("fixture-key".into()),
            models:Some(vec![maho_core::model_config_schema::ModelsJsonModel{id:id.into(),name:Some(id.into()),api:Some(CODEX_RESPONSES_API.into()),
                base_url:Some("https://example.invalid".into()),service_tier:tier,upstream_model_id:upstream.map(str::to_owned),..Default::default()}]),..Default::default()
        },..Default::default()}).expect("provider catalog");
    }
    let model=runtime.models().get_model("fixture","codex").expect("base model");
    let host=Arc::new(maho_cli::cli::default_extensions::test_service_tier_host());
    let extension=maho_ext_host::loader::NativeAsyncExtensionFactory{path:"<builtin:service-tier>".into(),source_info:Default::default(),factory:Arc::new(move|api|{
        let extension=ServiceTierExtension{host:host.clone()};Box::pin(async move{extension.register(api);Ok(())})
    })};
    let settings=SettingsManager::create(&cwd,&agent_dir.to_string_lossy(),&dir.to_string_lossy(),false);
    create_agent_session(CreateAgentSessionOptions{cwd:Some(cwd.clone()),agent_dir:Some(agent_dir.to_string_lossy().into_owned()),model_runtime:Some(runtime),model:Some(model),
        session_manager:Some(SessionManager::in_memory(&cwd,None,None)),settings_manager:Some(settings),extension_factories:vec![extension],no_tools:Some(NoToolsMode::All),auto_title_sessions:Some(false),..Default::default()
    }).await.expect("multi-provider session").session
}

#[tokio::test]
async fn production_host_persists_priority_and_auto_and_respects_priority_pin() {
    let dir=tempfile::tempdir().expect("isolated settings");
    let session=create_fast_session(dir.path(),"codex",None,catalog_models()).await;
    let settings_path=dir.path().join("agent/state.json");
    let command_context=session.extension_command_context().await.expect("session extension context");
    let on=command_context.context.apply_fast_mode(true).await.expect("enable");
    assert!(on.applied);
    assert_eq!(on.recorded_tier,ServiceTier::Priority);
    let written:serde_json::Value=serde_json::from_slice(&std::fs::read(&settings_path).expect("persisted model state")).expect("valid model state JSON");
    assert_eq!(written["modelServiceTiers"]["fixture/codex"],"priority");
    assert!(written["modelServiceTiers"]["fixture:codex"].is_null());
    let command_context=session.extension_command_context().await.expect("session extension context");
    let off=command_context.context.apply_fast_mode(false).await.expect("disable");
    assert!(off.applied);
    assert_eq!(off.recorded_tier,ServiceTier::Auto);
    let written:serde_json::Value=serde_json::from_slice(&std::fs::read(&settings_path).expect("persisted model state")).expect("valid model state JSON");
    assert_eq!(written["modelServiceTiers"]["fixture/codex"],"auto");
    assert!(written["modelServiceTiers"]["fixture:codex"].is_null());
    session.dispose().await;

    let pinned_dir=tempfile::tempdir().expect("pinned settings");
    let pinned=create_fast_session(pinned_dir.path(),"codex",Some(ServiceTier::Priority),catalog_models()).await;
    let pinned_settings_path=pinned_dir.path().join("agent/state.json");
    let before_bytes=std::fs::read(&pinned_settings_path).ok();
    let before=SettingsManager::create(&pinned_dir.path().to_string_lossy(),&pinned_dir.path().join("agent").to_string_lossy(),&pinned_dir.path().to_string_lossy(),false)
        .get_model_service_tier("fixture","codex").map(str::to_owned);
    let command_context=pinned.extension_command_context().await.expect("session extension context");
    let refused=command_context.context.apply_fast_mode(false).await.expect("priority refusal");
    assert!(!refused.applied);
    assert!(refused.enabled);
    let after_bytes=std::fs::read(&pinned_settings_path).ok();
    assert_eq!(after_bytes,before_bytes,"priority-pin refusal must not write settings");
    let after=SettingsManager::create(&pinned_dir.path().to_string_lossy(),&pinned_dir.path().join("agent").to_string_lossy(),&pinned_dir.path().to_string_lossy(),false)
        .get_model_service_tier("fixture","codex").map(str::to_owned);
    assert_eq!(after,before,"priority-pin refusal must not persist an auto override");
    pinned.dispose().await;
}

#[tokio::test]
async fn production_host_switches_only_to_compatible_fast_sibling() {
    let dir=tempfile::tempdir().expect("compatible catalog");
    let matching_catalog=catalog_models();
    assert_eq!(matching_catalog[1].upstream_model_id.as_deref(),Some("wire-codex"),"the fixture must deserialize upstreamModelId for the real host");
    let session=create_fast_session(dir.path(),"codex",None,matching_catalog).await;
    let command_context=session.extension_command_context().await.expect("session extension context");
    let enabled=command_context.context.apply_fast_mode(true).await.expect("compatible fast sibling");
    assert!(enabled.applied);
    assert_eq!(session.model().id,"codex-fast");
    session.dispose().await;

    let mismatch_dir=tempfile::tempdir().expect("mismatched catalog");
    let mut mismatch=catalog_models();
    mismatch[1].upstream_model_id=Some("wrong-wire-target".into());
    assert_ne!(mismatch[1].upstream_model_id,mismatch[0].upstream_model_id);
    let session=create_fast_session(mismatch_dir.path(),"codex",None,mismatch).await;
    let command_context=session.extension_command_context().await.expect("session extension context");
    let enabled=command_context.context.apply_fast_mode(true).await.expect("base model fast mode");
    assert!(enabled.applied);
    assert_eq!(session.model().id,"codex","different upstream model must not be selected as sibling");
    session.dispose().await;

    let priority_mismatch_dir=tempfile::tempdir().expect("non-priority sibling");
    let mut non_priority=catalog_models();
    non_priority[1].service_tier=None;
    let session=create_fast_session(priority_mismatch_dir.path(),"codex",None,non_priority).await;
    let command_context=session.extension_command_context().await.expect("session extension context");
    let enabled=command_context.context.apply_fast_mode(true).await.expect("base model without priority sibling");
    assert!(enabled.applied);
    assert_eq!(session.model().id,"codex","a sibling without catalog priority must not be selected");
    session.dispose().await;

    let provider_mismatch_dir=tempfile::tempdir().expect("provider-mismatched catalog");
    let session=create_fast_session_with_sibling_provider(provider_mismatch_dir.path()).await;
    let context=session.extension_command_context().await.expect("session extension context");
    let result=context.context.apply_fast_mode(true).await.expect("provider mismatch result");
    assert!(result.applied);
    assert_eq!(session.model().provider,"fixture");
    assert_eq!(session.model().id,"codex","fast sibling from another provider must be rejected");
    session.dispose().await;

    let api_mismatch_dir=tempfile::tempdir().expect("API-mismatched catalog");
    let mut api_models=catalog_models();
    api_models[1].api="openai-responses".into();
    let session=create_fast_session(api_mismatch_dir.path(),"codex",None,api_models).await;
    let context=session.extension_command_context().await.expect("session extension context");
    let result=context.context.apply_fast_mode(true).await.expect("API mismatch result");
    assert!(result.applied);
    assert_eq!(session.model().id,"codex","fast sibling with a different API must be rejected");
    session.dispose().await;
}
