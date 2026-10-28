#[cfg(unix)]
use maho_ext_anthropic_subscription::availability::probe_ambient_claude_auth_status;
use maho_ext_anthropic_subscription::{
    availability::{AMBIENT_PROBE_TIMEOUT_MS, AMBIENT_STATUS_TTL_MS},
    custom_tools::deny_custom_tool_execution,
    executable::{candidates, find_on_path, find_on_posix_path, ExecutableSource, ResolveInput},
    options::{build_query_configuration, QueryOptionsInput},
    prompt_append::{agents, skills},
    prompt_bridge::build_prompt_blocks,
    prompt_directive_dedupe::{DirectiveTag, dedupe_directive_blocks, serialized_payload_bytes},
    prompt_append::{project_rules, REGION_END, REGION_START},
    session_binding::{binding_from_stored_branch, binding_marker, stored_binding_from_entry, StoredBindingAnchor, BINDING_ENTRY_TYPE},
    session_continuity::{decide, prefix_digest, ContinuityInput, Decision, Snapshot},
    session_observability::{observe, PendingCloseCauses, SyncObservationInput},
    session_reattach::{abort_keeps_query, BindingStore},
    session_sync::{build_delta_prompt_blocks, config_fingerprint},
    stream_guidance::with_auth_guidance,
    system_prompt::load_override_prompt,
    stream_protocol::{map_stop_reason, update_usage, empty_output},
    tools::{map_tool_args, resolve_tools},
};
use std::collections::BTreeMap;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use serde_json::json;
use maho_ai::auth::types::{AuthContext, AuthPrompt, AuthPromptKind, AuthType, Credential, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
use maho_ext_anthropic_subscription::{accounts::{add_account, empty_credential, list_accounts, AccountSlot, AccountSource, SENTINEL_TOKEN}, oauth_login::AnthropicSubscriptionOAuth};
use maho_ext_anthropic_subscription::extension::AnthropicSubscriptionExtension;
use maho_ai::auth::types::CredentialStore;
use maho_ext_api::Extension;
use maho_ext_anthropic_subscription::guidance::version_floor_guidance;

/// Ties the returned input to the borrowed snapshot and hashes (closure lifetime elision cannot).
fn continuity_input_fn<F>(f: F) -> F where F: for<'a> Fn(&'a Snapshot, &'a [String]) -> ContinuityInput<'a> { f }

struct TestEnvironment(BTreeMap<String, String>);

#[async_trait::async_trait]
impl AuthContext for TestEnvironment {
    async fn env(&self, name: &str) -> Option<String> { self.0.get(name).cloned() }
    async fn file_exists(&self, _: &str) -> bool { false }
}

struct AuthFlow { logins: AtomicUsize }

#[async_trait::async_trait]
impl OAuthAuth for AuthFlow {
    fn name(&self) -> &str { "recovery-fixture" }
    async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.logins.fetch_add(1, Ordering::SeqCst);
        Ok(OAuthCredential::new("fresh-access", "fresh-refresh", 12.0))
    }
    async fn refresh(&self, credential: &OAuthCredential, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<OAuthCredential> { Ok(credential.clone()) }
    async fn to_auth(&self, _: &OAuthCredential) -> anyhow::Result<ModelAuth> { Ok(ModelAuth::default()) }
}

struct PromptAnswers { answers: std::sync::Mutex<std::collections::VecDeque<String>>, prompts: std::sync::Mutex<Vec<String>> }

#[async_trait::async_trait]
impl maho_ai::auth::types::AuthInteraction for PromptAnswers {
    fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> { None }
    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        if let AuthPromptKind::Text { message, .. } = prompt.kind { self.prompts.lock().expect("prompts").push(message); }
        Ok(self.answers.lock().expect("answers").pop_front().unwrap_or_default())
    }
    fn notify(&self, _: maho_ai::auth::types::AuthEvent) {}
}

fn oauth_adapter(settings: serde_json::Value, ambient: bool, store: Arc<maho_ai::auth::credential_store::InMemoryCredentialStore>) -> (AnthropicSubscriptionOAuth, Arc<AuthFlow>, Arc<AtomicUsize>) {
    let values = settings.as_object().expect("settings object").clone();
    let settings = Arc::new(move || maho_ext_anthropic_subscription::settings::ProviderSettings { values: values.clone(), prompt_mode_from_env: false });
    let probes=Arc::new(AtomicUsize::new(0));let observed=probes.clone();
    let reader=maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(move || {observed.fetch_add(1,Ordering::SeqCst);Box::pin(async move {Ok(ambient)})}),Arc::new(||0),30_000);
    let flow=Arc::new(AuthFlow {logins:AtomicUsize::new(0)});
    (AnthropicSubscriptionOAuth {store,flow:flow.clone(),settings,ambient:reader},flow,probes)
}

fn answers(values: &[&str]) -> (ProviderAuthInteraction, Arc<PromptAnswers>) {
    let scripted=Arc::new(PromptAnswers {answers:std::sync::Mutex::new(values.iter().map(|value|(*value).to_owned()).collect()),prompts:std::sync::Mutex::new(Vec::new())});
    let controller=maho_ai::utils::abort::AbortController::new();
    (ProviderAuthInteraction::new(controller.signal(),scripted.clone()),scripted)
}

async fn seed_credential(store:&Arc<maho_ai::auth::credential_store::InMemoryCredentialStore>, provider:&str, credential:OAuthCredential) {
    store.modify(provider,Box::new(move |_|{let credential=credential.clone();Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})}),None).await.expect("seed");
}

#[tokio::test]
async fn oauth_check_prefers_stored_and_environment_credentials_without_ambient_probe() {
    for (credential, environment) in [
        (Some(add_account(&empty_credential(), AccountSlot {name:"managed".into(),display_name:None,access:"access".into(),refresh:"refresh".into(),expires:0.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("managed")), BTreeMap::new()),
        (None, BTreeMap::from([("CLAUDE_CODE_OAUTH_TOKEN".into(),"env-token".into())])),
    ] {
        let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
        let (oauth,_,probes)=oauth_adapter(json!({}),false,store);
        let controller=maho_ai::utils::abort::AbortController::new();
        let check=oauth.check(&TestEnvironment(environment),credential.as_ref(),&controller.signal()).await.expect("check");
        assert!(check.as_ref().is_some_and(|check|check.auth_type==AuthType::OAuth&&check.source.as_deref()==Some("Anthropic Subscription")));
        assert_eq!(probes.load(Ordering::SeqCst),0);
    }
}

#[tokio::test]
async fn oauth_check_gates_ambient_probe_and_respects_explicit_ambient_lane() {
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let (oauth,_,probes)=oauth_adapter(json!({}),true,store.clone());
    let controller=maho_ai::utils::abort::AbortController::new();
    assert!(oauth.check(&TestEnvironment(BTreeMap::new()),None,&controller.signal()).await.expect("disabled check").is_none());
    assert_eq!(probes.load(Ordering::SeqCst),0);
    let (oauth,_,probes)=oauth_adapter(json!({"enabled":true,"tokenInjection":"ambient"}),true,store);
    let managed=add_account(&empty_credential(),AccountSlot {name:"managed".into(),display_name:None,access:"access".into(),refresh:"refresh".into(),expires:0.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("managed");
    assert!(oauth.check(&TestEnvironment(BTreeMap::new()),Some(&managed),&controller.signal()).await.expect("ambient check").is_some());
    assert_eq!(probes.load(Ordering::SeqCst),1);
}

#[tokio::test]
async fn oauth_login_imports_or_declines_existing_anthropic_credential() {
    for (answer, imported) in [("yes",true),("no",false)] {
        let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
        seed_credential(&store,"anthropic",OAuthCredential::new("old-access","old-refresh",1.0)).await;
        let (oauth,flow,_)=oauth_adapter(json!({}),false,store.clone());
        let (interaction,scripted)=answers(&[answer]);
        let credential=oauth.login(&interaction).await.expect("login");
        let slots=list_accounts(&credential,None).expect("accounts");
        if imported {
            assert_eq!(slots[0].name,"imported-anthropic");assert_eq!(slots[0].source,AccountSource::Import);
            assert!(store.read("anthropic",None).await.expect("anthropic slot").is_none());
            assert_eq!(flow.logins.load(Ordering::SeqCst),0);
        } else {
            assert_eq!(slots[0].name,"default");assert_eq!(slots[0].source,AccountSource::Login);
            assert!(store.read("anthropic",None).await.expect("anthropic slot").is_some());
            assert_eq!(flow.logins.load(Ordering::SeqCst),1);
        }
        assert!(!scripted.prompts.lock().expect("prompt capture").is_empty());
    }
}

#[tokio::test]
async fn oauth_login_adds_a_named_account_and_preserves_sentinel_fields() {
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let first=add_account(&empty_credential(),AccountSlot {name:"default".into(),display_name:None,access:"old".into(),refresh:"old-refresh".into(),expires:1.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("first slot");
    seed_credential(&store,maho_ext_anthropic_subscription::auth_lane::PROVIDER_ID,first).await;
    let (oauth,_,_)=oauth_adapter(json!({}),false,store);
    let (interaction,scripted)=answers(&["work"]);
    let credential=oauth.login(&interaction).await.expect("login");
    let slots=list_accounts(&credential,None).expect("accounts");
    assert_eq!(slots.iter().map(|slot|slot.name.as_str()).collect::<Vec<_>>(),["default","work"]);
    assert_eq!(credential.access,SENTINEL_TOKEN);assert_eq!(credential.refresh,SENTINEL_TOKEN);
    assert!(scripted.prompts.lock().expect("prompts")[0].contains("Name for this account"));
}

#[tokio::test]
async fn oauth_auth_uses_sentinel_and_refresh_preserves_credential() {
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let (oauth,_,_)=oauth_adapter(json!({}),false,store);
    let credential=empty_credential();let controller=maho_ai::utils::abort::AbortController::new();
    assert_eq!(oauth.to_auth(&credential).await.expect("auth").api_key.as_deref(),Some(SENTINEL_TOKEN));
    assert_eq!(oauth.refresh(&credential,&controller.signal()).await.expect("refresh"),credential);
}

#[tokio::test]
async fn ambient_resolution_uses_cli_request_tokens_and_preserves_empty_mask() {
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let (oauth,_,probes)=oauth_adapter(json!({"enabled":true}),true,store);
    let controller=maho_ai::utils::abort::AbortController::new();
    let cli=oauth.resolve_ambient(&TestEnvironment(BTreeMap::new()),None,&controller.signal()).await.expect("ambient").expect("auth result");
    assert_eq!(cli.auth.api_key.as_deref(),Some(SENTINEL_TOKEN));assert!(cli.env.is_none());
    let request=BTreeMap::from([("CLAUDE_CODE_OAUTH_TOKEN".to_owned(),"request-token".to_owned())]);
    let resolved=oauth.resolve_ambient(&TestEnvironment(BTreeMap::new()),Some(&request),&controller.signal()).await.expect("request token").expect("auth result");
    assert_eq!(resolved.env.expect("forwarded request token")["CLAUDE_CODE_OAUTH_TOKEN"],"request-token");
    let mask=BTreeMap::from([("CLAUDE_CODE_OAUTH_TOKEN".to_owned(),String::new())]);
    let resolved=oauth.resolve_ambient(&TestEnvironment(BTreeMap::new()),Some(&mask),&controller.signal()).await.expect("empty mask").expect("auth result");
    assert_eq!(resolved.env.expect("empty env mask")["CLAUDE_CODE_OAUTH_TOKEN"],"");
    assert_eq!(probes.load(Ordering::SeqCst),1);
}

fn provider_extension(settings: Arc<dyn Fn() -> maho_ext_anthropic_subscription::settings::ProviderSettings + Send + Sync>) -> AnthropicSubscriptionExtension {
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let ambient=maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(||Box::pin(async {Ok(false)})),Arc::new(||0),30_000);
    let oauth=Arc::new(AnthropicSubscriptionOAuth {store,flow:Arc::new(maho_ai::auth::oauth::anthropic::AnthropicOAuth::new(maho_ai::auth::oauth::transport::default_transport())),settings:settings.clone(),ambient});
    AnthropicSubscriptionExtension {oauth,stream:Arc::new(|_,_,_|panic!("registration does not stream")),settings,registry:Arc::new(tokio::sync::Mutex::new(maho_ext_anthropic_subscription::session_stream::SessionRegistry::default())),watch:Arc::new(std::sync::Mutex::new(maho_ext_anthropic_subscription::tool_watch::ToolWatch::default()))}
}

struct ProviderCapture(std::sync::Mutex<Vec<maho_ext_api::types::ProviderRegistration>>);

impl maho_ext_api::types::ExtensionProviderActions for ProviderCapture {
    fn register_provider(&self, registration:maho_ext_api::types::ProviderRegistration, _: &str) -> Result<(),maho_ext_api::types::ExtensionFailure> {
        self.0.lock().expect("capture registrations").push(registration);Ok(())
    }
    fn unregister_provider(&self, _: &str, _: &str) -> Result<(),maho_ext_api::types::ExtensionFailure> { Ok(()) }
}

#[test]
fn provider_registration_exposes_oauth_models_and_native_stream() {
    let settings=Arc::new(||maho_ext_anthropic_subscription::settings::ProviderSettings {values:serde_json::Map::new(),prompt_mode_from_env:false});
    let runtime=maho_ext_api::types::ExtensionRuntime::default();
    let mut api=maho_ext_api::types::ExtensionApi::new(maho_ext_api::types::LoadedExtension::new("anthropic-subscription","/tmp".into(),maho_ext_api::types::SourceInfo::default()),maho_ext_api::types::ExtensionSessionProfile::default(),maho_ext_api::types::EventBus::default(),runtime.clone());
    provider_extension(settings).register(&mut api);
    let capture=Arc::new(ProviderCapture(std::sync::Mutex::new(Vec::new())));runtime.bind_providers(capture.clone()).expect("bind");
    let registrations=capture.0.lock().expect("registrations");
    let maho_ext_api::types::ProviderRegistration::Config {name,config}=&registrations[0] else {panic!("provider config")};
    assert_eq!(name,maho_ext_anthropic_subscription::auth_lane::PROVIDER_ID);
    assert_eq!(config.base_url.as_deref(),Some(maho_ext_anthropic_subscription::api_id::CLAUDE_SDK_OAUTH_API_ID));
    assert!(!config.models.as_ref().expect("catalog").is_empty());
    assert!(config.oauth.is_some());assert!(config.stream_simple.is_some());
}

#[test]
fn provider_fallback_eligibility_tracks_the_enabled_false_kill_switch() {
    let state=Arc::new(std::sync::Mutex::new(None::<bool>));
    let settings={let state=state.clone();Arc::new(move || {let mut values=serde_json::Map::new();if let Some(enabled)=*state.lock().expect("state") {values.insert("enabled".into(),json!(enabled));}maho_ext_anthropic_subscription::settings::ProviderSettings {values,prompt_mode_from_env:false}})};
    let runtime=maho_ext_api::types::ExtensionRuntime::default();
    let mut api=maho_ext_api::types::ExtensionApi::new(maho_ext_api::types::LoadedExtension::new("anthropic-subscription","/tmp".into(),maho_ext_api::types::SourceInfo::default()),maho_ext_api::types::ExtensionSessionProfile::default(),maho_ext_api::types::EventBus::default(),runtime.clone());
    provider_extension(settings).register(&mut api);
    let capture=Arc::new(ProviderCapture(std::sync::Mutex::new(Vec::new())));runtime.bind_providers(capture.clone()).expect("bind");
    let registrations=capture.0.lock().expect("registrations");
    let maho_ext_api::types::ProviderRegistration::Config {config,..}=&registrations[0] else {panic!("provider config")};
    let eligible=config.fallback_eligible.as_ref().expect("fallback closure");
    assert!(eligible());*state.lock().expect("state")=Some(true);assert!(eligible());*state.lock().expect("state")=Some(false);assert!(!eligible());
}

#[test]
fn override_prompt_errors_include_configurable_target_and_missing_path() {
    let directory=tempfile::tempdir().expect("directory");
    let missing=directory.path().join("missing-prompt.md");
    let error=load_override_prompt(Some(missing.to_str().expect("path"))).expect_err("missing file");
    assert!(error.to_string().contains("missing-prompt.md"));
    let error=load_override_prompt(None).expect_err("missing setting");
    assert!(error.to_string().contains("systemPromptFile"));
}

#[test]
fn preset_append_composes_agent_skills_and_project_rule_blocks() {
    let directory=tempfile::tempdir().expect("directory");
    std::fs::write(directory.path().join("AGENTS.md"),"Use the senpi workspace.").expect("agents");
    let skill_prompt="The following skills provide specialized instructions for specific tasks.\n<available_skills>\n<skill>deploy</skill>\n</available_skills>";
    let rule="<project_rules>\n## Project Instructions\nShip carefully.\n</project_rules>";
    let prompt=format!("{skill_prompt}\n{REGION_START}\n{rule}\n{REGION_END}");
    let extracted_agents=agents(directory.path(),directory.path(),".omo").expect("agent block");
    let extracted_skills=skills(Some(&prompt),&directory.path().join("skills"),&directory.path().join(".omo/skills")).expect("skills block");
    let extracted_rules=project_rules(Some(&prompt)).expect("project rule");
    let settings=maho_ext_anthropic_subscription::settings::load(&json!({"anthropicSubscriptionProvider":{"systemPromptMode":"preset-append"}}),&serde_json::Value::Null,&Default::default());
    let model=json!({"id":"claude-sonnet-4-6"});let context=json!({"systemPrompt":prompt,"tools":[]});
    let options=build_query_configuration(QueryOptionsInput {model:&model,context:&context,stream_options:&json!({}),settings:&settings,lane:maho_ext_anthropic_subscription::auth_lane::TokenInjection::Ambient,cwd:directory.path(),agent_dir:directory.path(),config_directory:".omo",tools:None,executable:None}).expect("query configuration");
    let append=options["systemPrompt"]["append"].as_str().expect("preset append");
    for block in [extracted_agents,extracted_skills,extracted_rules.to_owned()] {assert!(append.contains(&block));}
}

#[test]
fn history_envelope_keeps_user_anchor_and_prompt_message_fields() {
    let blocks=build_prompt_blocks(&[json!({"role":"user","content":"Hello"})],&BTreeMap::new(),None);
    assert_eq!(blocks.len(),2);assert!(blocks[0]["text"].as_str().expect("anchor").starts_with("The above is the conversation history"));assert_eq!(blocks[1]["text"],"Hello");
}

#[test]
fn directive_payload_fixture_collapses_to_latest_and_reduces_size() {
    let body="x".repeat(20_000);let directive=format!("<focus-mode>{body}</focus-mode>");
    let blocks:Vec<_>=(0..10).map(|n|if n%2==0 {json!({"type":"text","text":directive})}else {json!({"type":"text","text":format!("plain user message {n}")})}).collect();
    let raw=serialized_payload_bytes(&blocks);let deduped=dedupe_directive_blocks(&blocks, &DirectiveTag::new("focus-mode", "focus"));
    assert_eq!(deduped.collapsed_directives,4);assert!(serialized_payload_bytes(&deduped.blocks)<raw);
    assert_eq!(deduped.blocks.iter().filter_map(|v|v["text"].as_str()).map(|s|s.matches("<focus-mode>").count()).sum::<usize>(),1);
}

#[test]
fn continuity_divergence_matrix_keeps_live_sessions_resumable() {
    let hashes=vec!["h1".into(),"h2".into(),"h3".into(),"h4".into()];
    let entry=Snapshot {sdk_session_id:"sdk".into(),sent_count:3,sent_hashes:hashes[..3].to_vec(),last_assistant_uuid:Some("a3".into()),assistant_uuid_by_index:BTreeMap::from([(1,"a1".into()),(2,"a2".into()),(3,"a3".into())]),account_name:"primary".into(),model_id:"model".into(),system_prompt_hash:"prompt".into(),toolset_hash:"tools".into(),..Default::default()};
    let request=continuity_input_fn(|entry:&Snapshot,current:&[String]|ContinuityInput {entry:Some(entry),binding:None,current_hashes:current,account_name:"primary",model_id:"model",system_prompt_hash:"prompt",toolset_hash:"tools",transcript_available:true,cross_account_resume_supported:true,idle_expired:false,invalidation_reason:None,credential_digest:None});
    for decision in [
        decide(&request(&entry,&hashes)),
        decide(&{let mut r=request(&entry,&hashes);r.account_name="other";r}),
        decide(&{let mut r=request(&entry,&hashes);r.model_id="other";r}),
        decide(&{let mut r=request(&entry,&hashes);r.system_prompt_hash="other";r}),
        decide(&request(&Snapshot {pending_fork_reason:Some("assistant_rewritten".into()),..entry.clone()},&hashes)),
        decide(&request(&Snapshot {tainted_reason:Some("abort".into()),..entry.clone()},&hashes)),
        decide(&request(&entry,&["h1".into(),"h2".into()])),
    ] {assert!(!matches!(decision,Decision::Flatten {..}));}
}

#[test]
fn observability_buckets_failure_vocabulary_and_cold_seed_fields() {
    for (reason,is_error,expected) in [("did not terminate",true,"abort_timeout"),("result arrived before replay claim",true,"turn_attribution_failed"),("Claude Code query ended before init",true,"query_failed"),("opaque failure",false,"other")] {
        assert_eq!(maho_ext_anthropic_subscription::session_continuity::sanitize_reason(reason),expected);
        if !is_error { assert_eq!(expected,"other"); }
    }
    let mut pending=PendingCloseCauses::default();
    let cold=observe(SyncObservationInput {kind:"cold-seed",reason:Some("transcript_missing"),delta_messages:5,first_turn:false,session:"telemetry",payload_bytes:Some(214_328),collapsed_directives:Some(4),settings_source:None,seat:None,seat_changed:None},&mut pending);
    assert_eq!(cold.payload_bytes,Some(214_328));assert_eq!(cold.collapsed_directives,Some(4));
    let delta=observe(SyncObservationInput {kind:"incremental",reason:None,delta_messages:3,first_turn:false,session:"telemetry",payload_bytes:Some(999),collapsed_directives:Some(9),settings_source:None,seat:None,seat_changed:None},&mut pending);
    assert_eq!(delta.payload_bytes,None);assert_eq!(delta.collapsed_directives,None);
}

#[cfg(unix)]
fn status_script(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("permissions");
    (directory, path)
}

#[cfg(unix)]
#[tokio::test]
async fn ambient_status_probe_uses_prompt_exit_and_rejects_failure_or_missing_binary() {
    let (_success_dir, success) = status_script("exit 0");
    assert!(probe_ambient_claude_auth_status(&success, 30_000).await);
    let (_failure_dir, failure) = status_script("exit 3");
    assert!(!probe_ambient_claude_auth_status(&failure, 30_000).await);
    let directory = tempfile::tempdir().expect("directory");
    assert!(!probe_ambient_claude_auth_status(&directory.path().join("missing"), 30_000).await);
}

#[test]
fn ambient_probe_deadline_and_status_cache_constants_are_stable() {
    assert_eq!(AMBIENT_PROBE_TIMEOUT_MS, 10_000);
    assert_eq!(AMBIENT_STATUS_TTL_MS, 30_000);
}

#[cfg(unix)]
#[tokio::test]
async fn ambient_status_probe_deadline_returns_unavailable_for_hung_child() {
    let (_directory, executable) = status_script("exec sleep 30");
    assert!(!probe_ambient_claude_auth_status(&executable, 1).await);
}

#[test]
fn custom_tool_denial_has_the_error_shape_and_shared_message() {
    let result = deny_custom_tool_execution();
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], maho_ext_anthropic_subscription::tools::TOOL_EXECUTION_DENIED_MESSAGE);
}

#[test]
fn posix_path_lookup_returns_none_when_path_is_unset() {
    assert_eq!(find_on_posix_path("claude", None), None);
}

#[test]
fn posix_path_lookup_uses_first_existing_regular_file_in_order() {
    let directory = tempfile::tempdir().expect("directory");
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir_all(&first).expect("first directory");
    std::fs::create_dir_all(&second).expect("second directory");
    let first_claude = first.join("claude");
    let second_claude = second.join("claude");
    std::fs::write(&first_claude, "").expect("first executable");
    std::fs::write(&second_claude, "").expect("second executable");
    let path = format!("{}:{}", first.display(), second.display());
    assert_eq!(find_on_posix_path("claude", Some(&path)), Some(first_claude));
}

#[test]
fn bundled_candidate_arch_matrix_preserves_linux_and_windows_order() {
    assert_eq!(candidates("darwin", "arm64", false), ["@anthropic-ai/claude-agent-sdk-darwin-arm64/claude"]);
    assert_eq!(candidates("linux", "x64", false), ["@anthropic-ai/claude-agent-sdk-linux-x64/claude", "@anthropic-ai/claude-agent-sdk-linux-x64-musl/claude"]);
    assert_eq!(candidates("linux", "arm64", true), ["@anthropic-ai/claude-agent-sdk-linux-arm64-musl/claude", "@anthropic-ai/claude-agent-sdk-linux-arm64/claude"]);
    assert_eq!(candidates("win32", "x64", false), ["@anthropic-ai/claude-agent-sdk-win32-x64/claude.exe"]);
}
#[test]
fn windows_path_search_skips_unreadable_batch_shims_and_continues() {
    let directory = tempfile::tempdir().expect("directory");
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir_all(&first).expect("first");
    std::fs::create_dir_all(&second).expect("second");
    let shim = first.join("claude.cmd");
    let native = second.join("claude.exe");
    std::fs::write(&shim, "batch").expect("shim");
    std::fs::write(&native, "exe").expect("native");
    let path = format!("{};{}", first.display(), second.display());
    let mut skipped = Vec::new();
    assert_eq!(find_on_path("claude", "win32", Some(&path), Some(".CMD;.EXE"), |p| skipped.push(p.to_path_buf())), Some(native));
    assert_eq!(skipped, [shim]);
}

#[test]
fn executable_resolution_miss_lists_bundled_candidates_and_path() {
    let resolution = maho_ext_anthropic_subscription::executable::describe(
        ResolveInput { platform: "linux", arch: "arm64", prefer_musl: false, override_path: None, path: None },
        |_| None,
        None,
        None,
    ).expect("resolution");
    assert_eq!(resolution.executable, None);
    assert_eq!(resolution.tried, [
        "@anthropic-ai/claude-agent-sdk-linux-arm64/claude",
        "@anthropic-ai/claude-agent-sdk-linux-arm64-musl/claude",
        "claude on PATH (PATH is unset)",
    ]);
}

#[test]
fn version_floor_guidance_selects_remedy_for_executable_source() {
    let text = "API Error: 400 Claude Code 2.1.241 does not support this model; version 2.1.251 or newer is required.";
    assert!(version_floor_guidance(text, Some(ExecutableSource::Override), "explicit").expect("override").contains("CLAUDE_CODE_EXECUTABLE (explicit)"));
    assert!(version_floor_guidance(text, Some(ExecutableSource::Bundled), "bundled").expect("bundled").contains("claude update` does not change it"));
    assert!(version_floor_guidance(text, Some(ExecutableSource::Path), "on-path").expect("path").contains("claude update` to get"));
    assert!(version_floor_guidance("nothing actionable", None, "unknown").is_none());
}

#[test]
fn version_floor_guidance_covers_unknown_source_and_model_not_found() {
    let version_floor="API Error: 400 Claude Code 2.1.241 does not support this model; version 2.1.251 or newer is required.";
    let unknown=version_floor_guidance(version_floor,None,"unknown").expect("unknown source guidance");
    assert!(unknown.contains("CLAUDE_CODE_EXECUTABLE"));assert!(unknown.contains("Claude Code 2.1.251 or newer"));assert!(!unknown.contains("<version>"));
    let model_missing=version_floor_guidance("model_not_found",None,"unknown").expect("model missing guidance");
    assert!(model_missing.contains("does not know this model id"));
}

#[test]
fn preset_append_project_rules_require_the_bounded_reserved_region() {
    let valid = "<project_rules>\n## Project Instructions\nrule\n</project_rules>";
    let prompt = format!("{REGION_START}decoy{REGION_END}\n{REGION_START}\n{valid}\n{REGION_END}");
    assert_eq!(project_rules(Some(&prompt)), Some(valid));
    assert_eq!(project_rules(Some(&format!("{REGION_START}{valid}"))), None);
}

#[test]
fn prompt_bridge_keeps_final_user_after_mixed_history_and_recovered_note() {
    let messages = [
        json!({"role":"user","content":"Find it"}),
        json!({"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"repoSearch","arguments":{"query":"needle"}}]}),
        json!({"role":"toolResult","toolCallId":"call-1","toolName":"repoSearch","content":[{"type":"text","text":"match"}]}),
        json!({"role":"user","content":"Explain the match"}),
    ];
    let custom = BTreeMap::from([("repoSearch".to_owned(), "mcp__custom-tools__repoSearch".to_owned())]);
    let blocks = build_prompt_blocks(&messages, &custom, Some(" recovered result "));
    let last_user = blocks.last().expect("final user");
    assert_eq!(last_user["text"], "Explain the match");
    assert!(blocks.iter().any(|block|block["text"].as_str().is_some_and(|text|text.contains("Historical tool call (non-executable)"))));
    let note = blocks.iter().position(|block|block["text"]=="recovered result").expect("note");
    let anchor = blocks.iter().position(|block|block["text"].as_str().is_some_and(|text|text.starts_with("The above is the conversation history"))).expect("anchor");
    assert!(note < anchor);
}

#[test]
fn whitespace_final_user_text_keeps_image_and_fallback_note() {
    let blocks = build_prompt_blocks(&[json!({"role":"user","content":[{"type":"text","text":"   "},{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})], &BTreeMap::new(), None);
    assert!(blocks.iter().any(|block|block["type"]=="image"&&block["source"]["media_type"]=="image/png"));
    assert!(blocks.iter().any(|block|block["text"]=="   "));
    assert!(blocks.iter().any(|block|block["text"]=="(see attached image)"));
}

#[test]
fn serialized_payload_bytes_sum_text_and_ignore_non_text_blocks() {
    let blocks = vec![json!({"type":"text","text":"한글😀"}),json!({"type":"text","text":"ascii"}),json!({"type":"image","source":{"data":"hidden"}})];
    assert_eq!(serialized_payload_bytes(&blocks), "한글😀".len()+5);
    assert_eq!(serialized_payload_bytes(&[]), 0);
}

#[test]
fn directive_deduplication_reduces_large_repeated_payload() {
    let body = "x".repeat(20_000);
    let directive = format!("<focus-mode>{body}</focus-mode>");
    let blocks: Vec<_> = (0..10).map(|i| if i%2==0 {json!({"type":"text","text":directive})} else {json!({"type":"text","text":format!("plain {i}")})}).collect();
    let raw = serialized_payload_bytes(&blocks);
    let result = dedupe_directive_blocks(&blocks, &DirectiveTag::new("focus-mode", "focus"));
    assert_eq!(result.collapsed_directives, 4);
    assert!(serialized_payload_bytes(&result.blocks) < raw);
    assert_eq!(result.blocks.iter().filter_map(|block|block["text"].as_str()).map(|text|text.matches("<focus-mode>").count()).sum::<usize>(), 1);
}

#[test]
fn binding_sidecar_size_is_independent_of_hash_history_length() {
    let hashes: Vec<String> = (0..10_000).map(|i|format!("hash-{i}")).collect();
    let entry = Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"test".into(),system_prompt_hash:"1".repeat(64),toolset_hash:"2".repeat(64),assistant_uuid_by_index:BTreeMap::from([(10_000,"uuid-last".into())]),..Default::default()};
    let stored = stored_binding_from_entry(&entry, &hashes, StoredBindingAnchor {session_path:"/tmp/session",session_id:"session",marker_entry_id:"marker",assistant_content_hash:&"3".repeat(64)});
    let serialized = serde_json::to_string(&stored).expect("serialized binding");
    assert_eq!(stored.sent_count, 10_000);
    assert_eq!(stored.last_assistant_uuid.as_deref(), Some("uuid-last"));
    assert!(!serialized.contains("sentHashes"));
    assert!(serialized.len() < 1024);
}

fn continuity_snapshot() -> Snapshot {
    Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"model".into(),system_prompt_hash:"prompt".into(),toolset_hash:"tools".into(),sent_count:2,sent_hashes:vec!["h1".into(),"h2".into()],last_assistant_uuid:Some("a2".into()),assistant_uuid_by_index:BTreeMap::from([(1,"a1".into()),(2,"a2".into())]),..Default::default()}
}

fn continuity_input<'a>(hashes:&'a [String])->ContinuityInput<'a> {
    ContinuityInput {entry:None,binding:None,current_hashes:hashes,account_name:"primary",model_id:"model",system_prompt_hash:"prompt",toolset_hash:"tools",transcript_available:true,cross_account_resume_supported:true,idle_expired:false,invalidation_reason:None,credential_digest:None}
}

#[test]
fn continuity_matrix_covers_bootstrap_delta_reattach_and_confirmation() {
    let hashes = vec!["h1".into(),"h2".into(),"h3".into()];
    let entry = continuity_snapshot();
    let mut request = continuity_input(&hashes); request.entry=Some(&entry);
    assert_eq!(decide(&request), Decision::Delta {from:2});
    assert_eq!(decide(&continuity_input(&hashes)), Decision::Bootstrap {reason:None});
    let mut binding=continuity_snapshot();binding.sent_hashes.truncate(2);
    let mut request=continuity_input(&hashes);request.binding=Some(&binding);
    assert_eq!(decide(&request), Decision::Reattach {sdk_session_id:"sdk".into(),from:2,reason:"registry_miss".into()});
    binding.sdk_session_id_confirmed=Some(false);let mut request=continuity_input(&hashes);request.binding=Some(&binding);
    assert_eq!(decide(&request), Decision::Flatten {reason:"session_unconfirmed".into()});
}

#[test]
fn continuity_retry_checkpoint_requires_matching_prefix_and_fingerprint_drift_reattaches() {
    let hashes=vec!["h1".into(),"h2".into(),"h3".into()];
    let mut binding=continuity_snapshot();binding.sent_prefix_hash=Some(prefix_digest(&hashes,2));binding.unanswered_turn_digest=Some(prefix_digest(&hashes,3));
    let mut request=continuity_input(&hashes);request.binding=Some(&binding);
    assert_eq!(decide(&request),Decision::Fork {sdk_session_id:"sdk".into(),at_uuid:"a2".into(),from:2,reason:"timeout_retry".into()});
    // Pinned decideFromBinding checks the retry checkpoint before prompt/toolset drift, so a
    // same-turn retry with drifted tools still forks at the checkpoint; with no checkpoint, the
    // matching prefix reattaches carrying the drift reason.
    let mut request=continuity_input(&hashes);request.binding=Some(&binding);request.toolset_hash="new-tools";
    assert_eq!(decide(&request),Decision::Fork {sdk_session_id:"sdk".into(),at_uuid:"a2".into(),from:2,reason:"timeout_retry".into()});
    binding.unanswered_turn_digest=None;
    let mut request=continuity_input(&hashes);request.binding=Some(&binding);request.toolset_hash="new-tools";
    assert_eq!(decide(&request),Decision::Reattach {sdk_session_id:"sdk".into(),from:2,reason:"toolset_changed".into()});
}

#[test]
fn retry_checkpoint_shadowing_cold_replays_only_on_matching_prefix() {
    let hashes=vec!["h1".to_owned(),"h2".to_owned(),"h3".to_owned()];
    let mut binding=Snapshot {sdk_session_id:"sdk".into(),sent_count:2,sent_hashes:vec!["h1".into(),"h2".into()],last_assistant_uuid:Some("a2".into()),account_name:"primary".into(),model_id:"model".into(),system_prompt_hash:"prompt".into(),toolset_hash:"tools".into(),unanswered_turn_digest:Some(prefix_digest(&hashes,3)),..Default::default()};
    let input=continuity_input_fn(|binding:&Snapshot,current:&[String]|ContinuityInput {entry:None,binding:Some(binding),current_hashes:current,account_name:"primary",model_id:"model",system_prompt_hash:"prompt",toolset_hash:"tools",transcript_available:true,cross_account_resume_supported:true,idle_expired:false,invalidation_reason:None,credential_digest:None});
    let with_prefix=Snapshot {sent_prefix_hash:Some(prefix_digest(&hashes,2)),..binding.clone()};
    assert!(matches!(decide(&input(&with_prefix,&hashes)),Decision::Fork {reason,..} if reason=="timeout_retry"));
    binding.sent_prefix_hash=Some(prefix_digest(&["h1".into(),"other".into()],2));
    assert_eq!(decide(&input(&binding,&hashes)),Decision::Flatten {reason:"sent_stream_diverged".into()});
}

#[test]
fn restart_binding_rejects_unconfirmed_session_and_reattaches_on_fingerprint_drift() {
    let hashes=vec!["h1".to_owned(),"h2".to_owned(),"h3".to_owned()];
    let mut binding=Snapshot {sdk_session_id:"sdk".into(),sent_count:2,sent_hashes:vec!["h1".into(),"h2".into()],account_name:"primary".into(),model_id:"model".into(),system_prompt_hash:"prompt".into(),toolset_hash:"tools".into(),..Default::default()};
    let input=continuity_input_fn(|binding:&Snapshot,current:&[String]|ContinuityInput {entry:None,binding:Some(binding),current_hashes:current,account_name:"primary",model_id:"model",system_prompt_hash:"changed-prompt",toolset_hash:"changed-tools",transcript_available:true,cross_account_resume_supported:true,idle_expired:false,invalidation_reason:None,credential_digest:None});
    assert_eq!(decide(&input(&binding,&hashes)),Decision::Reattach {sdk_session_id:"sdk".into(),from:2,reason:"system_prompt_changed".into()});
    binding.sdk_session_id_confirmed=Some(false);
    assert_eq!(decide(&input(&binding,&hashes)),Decision::Flatten {reason:"session_unconfirmed".into()});
}

#[test]
fn cold_seed_observations_project_telemetry_only_for_cold_seed() {
    let mut pending=PendingCloseCauses::default();
    let cold=observe(SyncObservationInput {kind:"cold-seed",reason:Some("registry_miss"),delta_messages:5,first_turn:true,session:"s",payload_bytes:Some(100),collapsed_directives:Some(4),settings_source:None,seat:None,seat_changed:None},&mut pending);
    assert_eq!(cold.kind,"bootstrap");assert_eq!(cold.payload_bytes,Some(100));assert_eq!(cold.collapsed_directives,Some(4));
    let delta=observe(SyncObservationInput {kind:"incremental",reason:None,delta_messages:2,first_turn:false,session:"s",payload_bytes:Some(500),collapsed_directives:Some(9),settings_source:None,seat:None,seat_changed:None},&mut pending);
    assert_eq!(delta.kind,"delta");assert_eq!(delta.payload_bytes,None);assert_eq!(delta.collapsed_directives,None);
}

#[test]
fn session_binding_accepts_model_visible_tail_and_abort_proof_is_conservative() {
    let marker=json!({"type":"custom","id":"marker","customType":BINDING_ENTRY_TYPE,"data":binding_marker()});
    let assistant=json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"test","content":[{"type":"text","text":"answer"}]});
    let hash=maho_ext_anthropic_subscription::session_commit_boundary::assistant_content_hash(&assistant);
    let entry=Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"test".into(),system_prompt_hash:"1".repeat(64),toolset_hash:"2".repeat(64),..Default::default()};
    let hashes=vec!["h1".into()];
    let stored=stored_binding_from_entry(&entry,&hashes,StoredBindingAnchor {session_path:"/tmp/session",session_id:"session",marker_entry_id:"marker",assistant_content_hash:&hash});
    let branch=vec![marker,json!({"type":"message","message":assistant}),json!({"type":"custom_message","customType":"unknown-extension"})];
    assert!(binding_from_stored_branch(&branch,&stored).is_some());
    assert!(abort_keeps_query(&json!({"still_queued":[]})));
    assert!(!abort_keeps_query(&json!({"still_queued":["uuid"]})));
    assert!(!abort_keeps_query(&json!({})));
}

#[test]
fn config_fingerprint_changes_for_working_directory_tools_and_executable() {
    let context=json!({"tools":[{"name":"read","description":"read","parameters":{"type":"object"}}]});
    let base=json!({"systemPrompt":"prompt","cwd":"/repo","tools":["Read"],"pathToClaudeCodeExecutable":"/bin/claude"});
    let fingerprint=|options:&serde_json::Value|config_fingerprint(options,&context,"ambient","default");
    let original=fingerprint(&base);
    let mut changed=base.clone();changed["cwd"]=json!("/other");assert_ne!(fingerprint(&changed).toolset_hash,original.toolset_hash);
    changed=base.clone();changed["pathToClaudeCodeExecutable"]=json!("/bin/other");assert_ne!(fingerprint(&changed).toolset_hash,original.toolset_hash);
    let changed_context=json!({"tools":[{"name":"write","description":"write","parameters":{"type":"object"}}]});
    assert_ne!(config_fingerprint(&base,&changed_context,"ambient","default").toolset_hash,original.toolset_hash);
}

#[test]
fn session_fingerprint_changes_when_semantic_prompt_tool_or_executable_changes() {
    let context=json!({"tools":[{"name":"read","description":"Read files","parameters":{"type":"object"}}]});
    let base=json!({"systemPrompt":"base prompt","cwd":"/repo","tools":["Read"],"pathToClaudeCodeExecutable":"/bin/claude"});
    let original=config_fingerprint(&base,&context,"oauth-slots","primary");
    let semantic=json!({"systemPrompt":"base prompt\nAlways answer concisely.","cwd":"/repo","tools":["Read"],"pathToClaudeCodeExecutable":"/bin/claude"});
    assert_ne!(config_fingerprint(&semantic,&context,"oauth-slots","primary").system_prompt_hash,original.system_prompt_hash);
    let changed_context=json!({"tools":[{"name":"read","description":"Different behavior","parameters":{"type":"object"}}]});
    assert_ne!(config_fingerprint(&base,&changed_context,"oauth-slots","primary").toolset_hash,original.toolset_hash);
    let changed_executable=json!({"systemPrompt":"base prompt","cwd":"/repo","tools":["Read"],"pathToClaudeCodeExecutable":"/bin/other"});
    assert_ne!(config_fingerprint(&changed_executable,&context,"oauth-slots","primary").toolset_hash,original.toolset_hash);
}

#[test]
fn host_denial_contract_matches_custom_tool_denial_without_turn_instruction() {
    let message=maho_ext_anthropic_subscription::tools::TOOL_EXECUTION_DENIED_MESSAGE;
    assert_eq!(maho_ext_anthropic_subscription::tools::deny_tool_execution(),json!({"behavior":"deny","message":message}));
    assert_eq!(message,maho_ext_anthropic_subscription::custom_tools::deny_custom_tool_execution()["content"][0]["text"]);
    assert!(!message.to_lowercase().contains("end the turn"));
    assert_eq!(maho_ext_anthropic_subscription::tools::CUSTOM_TOOLS_MCP_PREFIX,"mcp__custom-tools__");
}

#[test]
fn posix_lookup_skips_missing_and_directory_entries_before_first_file() {
    let directory=tempfile::tempdir().expect("directory");
    let missing=directory.path().join("missing");let as_dir=directory.path().join("directory");let hit=directory.path().join("hit");
    std::fs::create_dir(&as_dir).expect("directory path");std::fs::create_dir(&hit).expect("hit path");
    std::fs::write(hit.join("claude"),"binary").expect("executable");
    let path=format!("{}:{}:{}",missing.display(),as_dir.display(),hit.display());
    assert_eq!(find_on_posix_path("claude",Some(&path)),Some(hit.join("claude")));
}

#[test]
fn windows_search_uses_pathext_order_within_path_directories() {
    let directory=tempfile::tempdir().expect("directory");
    let first=directory.path().join("first");let second=directory.path().join("second");
    std::fs::create_dir_all(&first).expect("first");std::fs::create_dir_all(&second).expect("second");
    let later=first.join("claude.cmd");let earlier=first.join("claude.exe");let next_dir=second.join("claude.com");
    for path in [&later,&earlier,&next_dir] {std::fs::write(path,"file").expect("candidate");}
    let path=format!("{};{}",first.display(),second.display());
    assert_eq!(find_on_path("claude","win32",Some(&path),Some(".EXE;.CMD;.COM"),|_|{}),Some(earlier));
}

#[test]
fn batch_without_readable_native_shim_is_never_returned() {
    let directory=tempfile::tempdir().expect("directory");
    let batch=directory.path().join("claude.cmd");std::fs::write(&batch,"@node cli.js %*").expect("batch");
    let mut skipped=Vec::new();
    assert_eq!(find_on_path("claude","win32",directory.path().to_str(),Some(".CMD"),|path|skipped.push(path.to_path_buf())),None);
    assert_eq!(skipped,[batch]);
}

#[test]
fn tool_mapping_covers_edit_arguments_and_write_glob_matrix() {
    assert_eq!(map_tool_args("Edit",json!({"file_path":"src","old_string":"before","new_string":"after","replace_all":true}).as_object()),json!({"path":"src","edits":[{"oldText":"before","newText":"after"}]}).as_object().expect("expected").clone());
    assert_eq!(map_tool_args("Bash",json!({"command":"npm test","timeout":30000}).as_object())["timeout"].as_f64(),Some(30.0));
    let mapped=resolve_tools(Some(&[json!({"name":"repoSearch"})]));
    assert_eq!(mapped.to_pi.get("mcp__custom-tools__repoSearch").map(String::as_str),Some("repoSearch"));
    assert_eq!(mapped.custom_tools.len(),1);
}

#[test]
fn observed_terminal_failure_reason_vocabulary_is_sanitized() {
    for (input,expected) in [("did not terminate","abort_timeout"),("result arrived before replay claim","turn_attribution_failed"),("Claude Code query ended before init","query_failed"),("opaque error","other"),("tainted:abort","tainted_abort")] {
        assert_eq!(maho_ext_anthropic_subscription::session_continuity::sanitize_reason(input),expected);
    }
}

#[test]
fn telemetry_is_omitted_for_resume_and_delta_observations() {
    let mut pending=PendingCloseCauses::default();
    let resume=observe(SyncObservationInput {kind:"resume",reason:Some("history_rolled_back"),delta_messages:2,first_turn:false,session:"s",payload_bytes:Some(500),collapsed_directives:Some(3),settings_source:None,seat:None,seat_changed:None},&mut pending);
    assert_eq!(resume.kind,"fork");assert_eq!(resume.payload_bytes,None);assert_eq!(resume.collapsed_directives,None);
    let delta=observe(SyncObservationInput {kind:"incremental",reason:None,delta_messages:1,first_turn:false,session:"s",payload_bytes:Some(500),collapsed_directives:Some(3),settings_source:None,seat:None,seat_changed:None},&mut pending);
    assert_eq!(delta.kind,"delta");assert_eq!(delta.payload_bytes,None);assert_eq!(delta.collapsed_directives,None);
}

#[tokio::test]
async fn oauth_ambient_resolution_accepts_explicit_token_without_host_probe() {
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let (oauth,_,probes)=oauth_adapter(json!({"tokenInjection":"ambient"}),false,store);
    let controller=maho_ai::utils::abort::AbortController::new();
    let request=BTreeMap::from([("CLAUDE_CODE_OAUTH_TOKEN".to_owned(),"request-token".to_owned())]);
    let resolved=oauth.resolve_ambient(&TestEnvironment(BTreeMap::new()),Some(&request),&controller.signal()).await.expect("resolve request").expect("auth result");
    assert_eq!(resolved.env.expect("token env")["CLAUDE_CODE_OAUTH_TOKEN"],"request-token");
    assert_eq!(probes.load(Ordering::SeqCst),0);
}

#[test]
fn session_binding_only_allows_ledger_entries_before_assistant_anchor() {
    let assistant=json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"m","content":[{"type":"text","text":"ok"}]});
    let hash=maho_ext_anthropic_subscription::session_commit_boundary::assistant_content_hash(&assistant);
    let snapshot=Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"m".into(),system_prompt_hash:"p".into(),toolset_hash:"t".into(),..Default::default()};
    let hashes=vec!["h".into()];
    let stored=stored_binding_from_entry(&snapshot,&hashes,StoredBindingAnchor {session_path:"/tmp/session",session_id:"s",marker_entry_id:"marker",assistant_content_hash:&hash});
    let marker=json!({"type":"custom","id":"marker","customType":BINDING_ENTRY_TYPE,"data":binding_marker()});
    let mut valid=vec![marker.clone(),json!({"type":"label"}),json!({"type":"message","message":assistant.clone()})];
    assert!(binding_from_stored_branch(&valid,&stored).is_some());
    valid.insert(1,json!({"type":"custom_message","customType":"model-visible"}));
    assert!(binding_from_stored_branch(&valid,&stored).is_none());
}

#[test]
fn session_binding_newest_invalidation_and_marker_control_restoration() {
    let assistant=json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"m","content":[{"type":"text","text":"ok"}]});
    let hash=maho_ext_anthropic_subscription::session_commit_boundary::assistant_content_hash(&assistant);
    let snapshot=Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"m".into(),system_prompt_hash:"p".into(),toolset_hash:"t".into(),..Default::default()};
    let hashes=vec!["h".into()];
    let stored=stored_binding_from_entry(&snapshot,&hashes,StoredBindingAnchor {session_path:"/tmp/session",session_id:"s",marker_entry_id:"marker",assistant_content_hash:&hash});
    let marker=json!({"type":"custom","id":"marker","customType":BINDING_ENTRY_TYPE,"data":binding_marker()});
    let invalid=json!({"type":"custom","id":"invalid","customType":BINDING_ENTRY_TYPE,"data":{"invalidated":true,"reason":"changed"}});
    let mut branch=vec![marker.clone(),json!({"type":"message","message":assistant}),invalid];
    assert_eq!(maho_ext_anthropic_subscription::session_binding::invalidation_reason_from_branch(&branch),Some("changed"));
    assert!(binding_from_stored_branch(&branch,&stored).is_none());
    branch.push(marker);
    assert_eq!(maho_ext_anthropic_subscription::session_binding::invalidation_reason_from_branch(&branch),None);
}

#[test]
fn prompt_history_envelope_rules_cover_user_assistant_and_image_only_contexts() {
    let assistant=build_prompt_blocks(&[json!({"role":"assistant","content":[{"type":"text","text":"Earlier answer"}]})],&BTreeMap::new(),None);
    assert_eq!(assistant[0]["text"],"<conversation_history>\n");
    assert_eq!(assistant[2]["text"],"Earlier answer");
    assert!(assistant[4]["text"].as_str().expect("anchor").starts_with("The above is the conversation history"));
    let image=build_prompt_blocks(&[json!({"role":"user","content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})],&BTreeMap::new(),None);
    assert_eq!(image[1]["type"],"image");assert_eq!(image[2]["text"],"(see attached image)");
}

#[test]
fn delta_bridge_renders_malformed_content_with_flatten_bridge_placeholders() {
    let custom=BTreeMap::new();
    for (content,expected) in [
        (json!([{"type":"image","mimeType":"image/png"}]),"[image block omitted: missing data]"),
        (json!([{"type":"tool_use","id":"x"}]),"[unsupported content block omitted: tool_use]"),
        (json!([{"type":"text","text":42}]),"[unsupported content block omitted: text]"),
        (json!([{"type":"image","mimeType":"image/svg+xml","data":"PHN2Zz4="}]),"[image block omitted: unsupported media type image/svg+xml]"),
    ] {
        let blocks=build_delta_prompt_blocks(&[json!({"role":"user","content":content})],&custom);
        assert!(blocks.iter().any(|block|block["text"]==expected));
        assert!(blocks.iter().all(|block|block["type"]!="image"));
    }
    let image=build_delta_prompt_blocks(&[json!({"role":"user","content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})],&custom);
    assert!(image.iter().any(|block|block["type"]=="image"&&block["source"]["media_type"]=="image/png"));
}

#[test]
fn session_binding_rejects_rewritten_assistant_and_invalid_marker() {
    let assistant=json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"model","content":[{"type":"text","text":"answer"}]});
    let hash=maho_ext_anthropic_subscription::session_commit_boundary::assistant_content_hash(&assistant);
    let snapshot=Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"model".into(),system_prompt_hash:"prompt".into(),toolset_hash:"tools".into(),assistant_uuid_by_index:BTreeMap::from([(1,"a1".into())]),..Default::default()};
    let hashes=vec!["h1".to_owned()];
    let stored=stored_binding_from_entry(&snapshot,&hashes,StoredBindingAnchor {session_path:"/tmp/session",session_id:"session",marker_entry_id:"marker",assistant_content_hash:&hash});
    let marker=json!({"type":"custom","id":"marker","customType":BINDING_ENTRY_TYPE,"data":binding_marker()});
    let mut branch=vec![marker.clone(),json!({"type":"message","message":assistant.clone()})];
    assert!(binding_from_stored_branch(&branch,&stored).is_some());
    branch[0]["id"]=json!("different");assert!(binding_from_stored_branch(&branch,&stored).is_none());
    branch=vec![marker,json!({"type":"message","message":{"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"model","content":[{"type":"text","text":"rewritten"}]}})];
    assert!(binding_from_stored_branch(&branch,&stored).is_none());
}

#[test]
fn binding_clone_and_lineage_options_preserve_private_state_and_drop_minted_id() {
    let snapshot=Snapshot {sdk_session_id:"sdk".into(),sent_hashes:vec!["h1".into(),"h2".into()],assistant_uuid_by_index:BTreeMap::from([(1,"a1".into())]),..Default::default()};
    let mut store=BindingStore::default();store.remember("session",&snapshot);
    let mut copy=store.get("session").expect("binding");copy.sent_hashes[0]="modified".into();copy.assistant_uuid_by_index.insert(1,"modified".into());
    let second=store.get("session").expect("binding remains isolated");assert_eq!(second.sent_hashes[0],"h1");assert_eq!(second.assistant_uuid_by_index[&1],"a1");
    let options=maho_ext_anthropic_subscription::session_reattach::lineage_options(&snapshot,Some("a1"),&json!({"sessionId":"minted","model":"m"}));
    assert!(options.get("sessionId").is_none());assert_eq!(options["resume"],"sdk");assert_eq!(options["resumeSessionAt"],"a1");assert_eq!(options["forkSession"],true);
}

#[test]
fn stream_guidance_prioritizes_blocked_auth_and_appends_entitlement_guidance() {
    let blocked=anyhow::Error::new(maho_ext_anthropic_subscription::affinity::AllAccountsBlockedError {soonest_unblock_at:Some(1.0),block_reason:Some("auth_error"),unavailable_account:None});
    let text=with_auth_guidance(&blocked);
    assert!(text.starts_with("All Claude accounts for anthropic-subscription are currently blocked (authentication error)."));
    let message="Fable 5 requires usage credits. Run /usage-credits to continue";
    let guidance=with_auth_guidance(&anyhow::Error::msg(message));
    assert!(guidance.starts_with(message));
    assert!(guidance.contains("This model needs usage credits on the selected Claude account"));
}

#[test]
fn stream_protocol_helpers_map_stop_reasons_and_tools() {
    assert_eq!(map_stop_reason(Some("tool_use")),maho_ai::types::StopReason::ToolUse);
    assert_eq!(map_stop_reason(Some("max_tokens")),maho_ai::types::StopReason::Length);
    let model=maho_ai::models_generated::MODELS["anthropic"].values().next().expect("model").clone();
    let mut output=empty_output(&model,1);
    update_usage(&model,&mut output,&json!({"input_tokens":3,"output_tokens":2}),maho_ext_anthropic_subscription::stream_protocol::UsageScope::Query);
    assert_eq!(output.usage.input,3);assert_eq!(output.usage.output,2);assert_eq!(output.usage.total_tokens,5);
}

#[test]
fn full_prompt_and_override_file_are_projected_by_query_options() {
    let directory = tempfile::tempdir().expect("directory");
    let prompt = "stable\nCurrent date: decoy\nstill stable";
    let settings = maho_ext_anthropic_subscription::settings::load(&json!({}), &serde_json::Value::Null, &Default::default());
    let model = json!({"id":"claude-sonnet-4-6"});
    let context = json!({"systemPrompt":prompt,"tools":[]});
    let options = build_query_configuration(QueryOptionsInput { model:&model,context:&context,stream_options:&json!({}),settings:&settings,lane:maho_ext_anthropic_subscription::auth_lane::TokenInjection::Ambient,cwd:directory.path(),agent_dir:directory.path(),config_directory:".senpi",tools:None,executable:None }).expect("full options");
    assert_eq!(options["systemPrompt"], prompt);
    let override_file = directory.path().join("prompt.md");
    std::fs::write(&override_file, "override prompt").expect("override prompt");
    let settings = maho_ext_anthropic_subscription::settings::load(&json!({"anthropicSubscriptionProvider":{"systemPromptMode":"override","systemPromptFile":override_file.to_str().expect("override path")}}), &serde_json::Value::Null, &Default::default());
    let options = build_query_configuration(QueryOptionsInput { model:&model,context:&context,stream_options:&json!({}),settings:&settings,lane:maho_ext_anthropic_subscription::auth_lane::TokenInjection::Ambient,cwd:directory.path(),agent_dir:directory.path(),config_directory:".senpi",tools:None,executable:None }).expect("override options");
    assert_eq!(options["systemPrompt"], "override prompt");
}

#[test]
fn malformed_provider_settings_fall_back_to_default_tool_options() {
    let settings=maho_ext_anthropic_subscription::settings::load(&json!({"anthropicSubscriptionProvider":{"appendSystemPrompt":"false","settingSources":["user","gateway"],"strictMcpConfig":"true","pinnedAccount":7,"tokenInjection":"unmanaged"}}),&serde_json::Value::Null,&Default::default());
    let options=maho_ext_anthropic_subscription::options::tool_options(&settings,false,false,None);
    assert!(!options.contains_key("extraArgs"));
    assert_eq!(options["tools"],json!(["Read","Write","Edit","Bash","Grep","Glob"]));
}
