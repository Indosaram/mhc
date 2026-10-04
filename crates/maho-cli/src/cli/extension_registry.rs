//! Canonical native extension assembly for the `mhc` binary.
//!
//! Ports senpi `src/extensions/index.ts` (`builtInExtensions`) and
//! `src/core/extensions/builtin/index.ts` (`globalDefaultExtensionIds`, the ordered
//! `builtinExtensions` list). The pinned id order is data and is preserved exactly; every id is
//! bound to the real constructor of its owning crate when that constructor exists without host
//! state. Ids whose constructor needs per-session host state are listed in
//! [`deferred_builtin_extensions`] / [`deferred_user_extensions`] with the exact contract.
//!
//! Registration runs through `maho_ext_host::loader::load_extensions`, the same
//! `register` -> `commit_registration` path a disk-loaded factory takes, with failure isolation per
//! factory.

use std::path::Path;
use std::sync::Arc;

use maho_ext_api::{Extension, ExtensionSessionProfile, SourceInfo};
use maho_ext_host::loader::{load_extensions, LoadExtensionsResult, NativeExtensionFactory};

/// senpi `globalDefaultExtensionIds` (`core/extensions/builtin/index.ts`).
pub const GLOBAL_DEFAULT_EXTENSION_IDS: [&str; 4] = ["diff", "files", "prompt-url-widget", "tps"];

/// Pinned `builtinExtensions` id order (`core/extensions/builtin/index.ts`).
pub const BUILTIN_EXTENSION_IDS: [&str; 43] = [
    "loop-guard",
    "hooks",
    "permission-system",
    "gpt-apply-patch",
    "ask-user",
    "herdr",
    "imagegen",
    "openai-image-gen",
    "prompt-preset",
    "todowrite",
    "redraws",
    "anthropic-web-search",
    "anthropic-bash",
    "openai-web-search",
    "service-tier",
    "reasoning",
    "model-fallback",
    "recommended-models",
    "bash-timeout",
    "terminal",
    "tool-pair-guard",
    "compaction",
    "history-search",
    "help",
    "import-repro",
    "websearch",
    "webfetch",
    "video-in",
    "look-at",
    "nested-agents-md",
    "rules",
    "goal",
    "loop",
    "cache-keepalive",
    "ttsr",
    "btw",
    "account",
    "gpt-account",
    "claude-sdk-oauth",
    "cursor-cli-oauth",
    "config-reload",
    "tool-search",
    "mcp",
];

/// A pinned extension id bound to the owning crate's real constructor.
pub struct NativeExtension {
    pub id: &'static str,
    pub crate_name: &'static str,
    pub factory: fn() -> Box<dyn Extension>,
}

/// A pinned extension id whose constructor needs per-session host state or a missing owner API.
pub struct DeferredExtension {
    pub id: &'static str,
    pub crate_name: &'static str,
    pub requirement: &'static str,
}

fn loop_guard() -> Box<dyn Extension> {
    Box::new(maho_ext_loop_guard::LoopGuardExtension)
}
fn hooks() -> Box<dyn Extension> {
    Box::new(maho_ext_hooks::HooksExtension)
}
fn permission_system() -> Box<dyn Extension> {
    Box::new(maho_ext_permission_system::PermissionSystem)
}
fn herdr() -> Box<dyn Extension> {
    Box::new(maho_ext_herdr::reporter::Herdr)
}
fn prompt_preset() -> Box<dyn Extension> {
    Box::new(maho_ext_prompt_preset::PromptPreset)
}
fn anthropic_web_search() -> Box<dyn Extension> {
    Box::new(maho_ext_anthropic_web_search::AnthropicWebSearch)
}
fn anthropic_bash() -> Box<dyn Extension> {
    Box::new(maho_ext_anthropic_bash::AnthropicBash)
}
fn openai_web_search() -> Box<dyn Extension> {
    Box::new(maho_ext_openai_web_search::OpenAiWebSearch)
}
fn model_fallback() -> Box<dyn Extension> {
    Box::new(maho_ext_model_fallback::ModelFallback { is_using_oauth: Arc::new(|model| {
        maho_core::auth_storage::AuthStorage::create_default().get(&model.provider).as_ref()
            .and_then(maho_core::auth_storage::credential_kind) == Some(maho_core::auth_storage::CredentialKind::Oauth)
    }) })
}
fn recommended_models() -> Box<dyn Extension> {
    Box::new(maho_ext_recommended_models::RecommendedModels)
}
fn bash_timeout() -> Box<dyn Extension> {
    Box::new(maho_ext_bash_timeout::BashTimeout)
}
fn terminal() -> Box<dyn Extension> {
    Box::new(maho_ext_terminal::TerminalExtension)
}
fn tool_pair_guard() -> Box<dyn Extension> {
    Box::new(maho_ext_tool_pair_guard::ToolPairGuard)
}
fn compaction() -> Box<dyn Extension> {
    Box::new(maho_ext_compaction::CompactionExtension)
}
fn help() -> Box<dyn Extension> {
    Box::new(maho_ext_help::Help { display: Arc::new(|ctx, markdown| Box::pin(async move {
        ctx.ui.select(&markdown, &["Close".into()], Default::default()).await; Ok(())
    })) })
}
fn webfetch() -> Box<dyn Extension> {
    Box::new(maho_ext_webfetch::index::WebfetchExtension)
}
fn video_in() -> Box<dyn Extension> {
    Box::new(maho_ext_video_in::VideoIn)
}
fn nested_agents_md() -> Box<dyn Extension> {
    Box::new(maho_ext_nested_agents_md::NestedAgentsMd)
}
fn rules() -> Box<dyn Extension> {
    Box::new(maho_ext_rules::Rules)
}

/// `goal` binds its store through the owning crate's context helper
/// (`store_ref::goal_store_ref_from_context`).
fn goal() -> Box<dyn Extension> {
    let reference: maho_ext_goal::accounting_hooks::GoalStoreReference = Arc::new(
        |context: &maho_ext_api::ExtensionContext| maho_ext_goal::store_ref::context_goal_store_ref(context),
    );
    Box::new(maho_ext_goal::GoalExtension::new(reference))
}

/// `loop` binds its store through the owning crate's context helper
/// (`maho_ext_loop::store::loop_store_ref_from_context`); the closure returns the concrete ref the
/// controller requires, falling back to the helper's own no-session namespace only when a persisted
/// session's directory is unresolved.
fn loop_extension() -> Box<dyn Extension> {
    let reference: maho_ext_loop::controller::LoopStoreReference = Arc::new(
        |context: &maho_ext_api::ExtensionContext| {
            maho_ext_loop::store::loop_store_ref_from_context(context).unwrap_or_else(|| {
                maho_ext_loop::types::LoopStoreRef {
                    base_dir: std::path::PathBuf::from(maho_core::config::get_agent_dir())
                        .join("extensions")
                        .join("loop")
                        .join("no-session"),
                    session_id: context.session_manager.session_id().to_owned(),
                }
            })
        },
    );
    Box::new(maho_ext_loop::LoopExtension::new(reference))
}

/// `websearch` takes its config root and the provider-native bypass probe.
fn websearch() -> Box<dyn Extension> {
    let home = std::path::PathBuf::from(maho_core::config::home_dir());
    let provider_native_bypass: maho_ext_websearch::index::ProviderNativeBypass = Arc::new(|_| false);
    Box::new(maho_ext_websearch::WebsearchExtension { home, provider_native_bypass })
}

/// Linked builtin factories, in the pinned `builtinExtensions` relative order.
static BUILTIN_FACTORIES: [NativeExtension; 22] = [
    NativeExtension { id: "loop-guard", crate_name: "maho-ext-loop-guard", factory: loop_guard },
    NativeExtension { id: "hooks", crate_name: "maho-ext-hooks", factory: hooks },
    NativeExtension { id: "permission-system", crate_name: "maho-ext-permission-system", factory: permission_system },
    NativeExtension { id: "herdr", crate_name: "maho-ext-herdr", factory: herdr },
    NativeExtension { id: "prompt-preset", crate_name: "maho-ext-prompt-preset", factory: prompt_preset },
    NativeExtension { id: "anthropic-web-search", crate_name: "maho-ext-anthropic-web-search", factory: anthropic_web_search },
    NativeExtension { id: "anthropic-bash", crate_name: "maho-ext-anthropic-bash", factory: anthropic_bash },
    NativeExtension { id: "openai-web-search", crate_name: "maho-ext-openai-web-search", factory: openai_web_search },
    NativeExtension { id: "model-fallback", crate_name: "maho-ext-model-fallback", factory: model_fallback },
    NativeExtension { id: "recommended-models", crate_name: "maho-ext-recommended-models", factory: recommended_models },
    NativeExtension { id: "bash-timeout", crate_name: "maho-ext-bash-timeout", factory: bash_timeout },
    NativeExtension { id: "terminal", crate_name: "maho-ext-terminal", factory: terminal },
    NativeExtension { id: "tool-pair-guard", crate_name: "maho-ext-tool-pair-guard", factory: tool_pair_guard },
    NativeExtension { id: "compaction", crate_name: "maho-ext-compaction", factory: compaction },
    NativeExtension { id: "help", crate_name: "maho-ext-help", factory: help },
    NativeExtension { id: "websearch", crate_name: "maho-ext-websearch", factory: websearch },
    NativeExtension { id: "webfetch", crate_name: "maho-ext-webfetch", factory: webfetch },
    NativeExtension { id: "video-in", crate_name: "maho-ext-video-in", factory: video_in },
    NativeExtension { id: "nested-agents-md", crate_name: "maho-ext-nested-agents-md", factory: nested_agents_md },
    NativeExtension { id: "rules", crate_name: "maho-ext-rules", factory: rules },
    NativeExtension { id: "goal", crate_name: "maho-ext-goal", factory: goal },
    NativeExtension { id: "loop", crate_name: "maho-ext-loop", factory: loop_extension },
];

pub fn builtin_extensions() -> &'static [NativeExtension] {
    &BUILTIN_FACTORIES
}

/// Pinned builtin ids whose constructor needs host state the CLI cannot build at registration.
static DEFERRED_BUILTINS: [DeferredExtension; 23] = [
    DeferredExtension { id: "gpt-apply-patch", crate_name: "maho-ext-gpt-apply-patch", requirement: "crate publishes tool internals only; needs the extension factory for the apply-patch toolset" },
    DeferredExtension { id: "ask-user", crate_name: "maho-ext-ask-user", requirement: "crate publishes schema/pending/format/resume/params/render modules only; needs the ask-user `Extension` factory" },
    DeferredExtension { id: "imagegen", crate_name: "maho-ext-imagegen", requirement: "crate publishes paths/params/state/tool internals; needs the imagegen `Extension` factory" },
    DeferredExtension { id: "openai-image-gen", crate_name: "maho-ext-openai-image-gen", requirement: "crate publishes gate/inject/externalize internals; needs the `Extension` factory" },
    DeferredExtension { id: "todowrite", crate_name: "maho-ext-todotools", requirement: "`TodotoolsExtension { actions: Arc<dyn ExtensionActions>, accessors: Arc<dyn TodoAccessors>, copy_markdown }` needs both handles: the core `Arc<dyn ExtensionActions>` and a session-state `TodoAccessors` (the crate ships only the `ExecutionFixture` test impl; the host must own the current-phases state + widget sync)" },
    DeferredExtension { id: "redraws", crate_name: "maho-ext-builtin-loose", requirement: "crate publishes the Tps extension only; needs the redraws `Extension`" },
    DeferredExtension { id: "service-tier", crate_name: "maho-ext-builtin-loose", requirement: "crate publishes the Tps extension only; needs the service-tier `Extension`" },
    DeferredExtension { id: "reasoning", crate_name: "maho-ext-reasoning", requirement: "crate publishes no `impl Extension`; needs the reasoning lane factory" },
    DeferredExtension { id: "history-search", crate_name: "maho-ext-history-search", requirement: "crate publishes overlay/indexer internals; needs the `Extension` factory" },
    DeferredExtension { id: "import-repro", crate_name: "maho-ext-builtin-loose", requirement: "crate publishes the Tps extension only; needs the import-repro `Extension`" },
    DeferredExtension { id: "look-at", crate_name: "maho-ext-look-at", requirement: "crate publishes runner/settings internals; needs the `Extension` factory" },
    DeferredExtension { id: "cache-keepalive", crate_name: "maho-ext-cache-keepalive", requirement: "crate publishes helpers only; needs the `Extension` factory" },
    DeferredExtension { id: "btw", crate_name: "maho-ext-btw", requirement: "crate publishes `side_query` only; needs the btw `Extension`" },
    DeferredExtension { id: "account", crate_name: "maho-ext-account", requirement: "crate publishes command parsing only; needs the account `Extension`" },
    DeferredExtension { id: "gpt-account", crate_name: "maho-ext-builtin-loose", requirement: "crate publishes the Tps extension only; needs the gpt-account `Extension`" },
    DeferredExtension { id: "claude-sdk-oauth", crate_name: "maho-ext-anthropic-subscription", requirement: "host-constructed, not static: `crate::cli::oauth_providers::oauth_extension_factories` builds it from the resolved Claude executable + auth.json store + settings; static assembly cannot supply that host state" },
    DeferredExtension { id: "cursor-cli-oauth", crate_name: "maho-ext-cursor-cli-oauth", requirement: "host-constructed via `crate::cli::oauth_providers::cursor_cli_extension` from lane-24's real `CursorCliOAuth::native(store, settings, resolve, persist_acknowledgement, persist_enabled, now)` + `production_*` helpers; static assembly cannot supply the resolved `cursor-agent` executable, the settings loader or the SettingsStorage" },
    DeferredExtension { id: "tool-search", crate_name: "maho-ext-tool-search", requirement: "host-constructed, not static: the CLI builds `ToolSearchExtension` with the MCP native gate closure (`crate::cli::default_extensions`) so the shared tool-search service and the MCP gate read the same resolved state; static assembly cannot supply that per-session gate" },
    DeferredExtension { id: "mcp", crate_name: "maho-ext-mcp", requirement: "host-constructed, not static: the CLI builds `maho_ext_mcp::index::McpExtension { registry, owner: 1, tool_search: Some(shared) }` over the `crate::cli::tool_search::SharedToolSearch` service and reads the returned service's native tool-search gate (`crate::cli::default_extensions`); static assembly cannot supply that per-session shared service" },
    DeferredExtension { id: "diff", crate_name: "maho-ext-builtin-loose", requirement: "`globalDefaultExtensionIds` member; crate publishes `diff::FileInfo` only, needs the diff `Extension`" },
    DeferredExtension { id: "files", crate_name: "maho-ext-builtin-loose", requirement: "`globalDefaultExtensionIds` member; crate publishes `files::FileEntry` only, needs the files `Extension`" },
    DeferredExtension { id: "ttsr", crate_name: "maho-ext-ttsr", requirement: "this worktree's lib.rs is empty; needs the TtsrExtension constructor from its owner" },
    DeferredExtension { id: "config-reload", crate_name: "maho-ext-config-reload", requirement: "this worktree's lib.rs is empty; needs the ConfigReload constructor from its owner" },
];

pub fn deferred_builtin_extensions() -> &'static [DeferredExtension] {
    &DEFERRED_BUILTINS
}

/// senpi `src/extensions/index.ts` `builtInExtensions`: inline, hidden.
pub const INLINE_EXTENSION_IDS: [&str; 1] = ["llama.cpp"];

fn ferryx_agent_state() -> Box<dyn Extension> {
    Box::new(maho_ext_ferryx_agent_state::FerryxAgentState)
}
fn herdr_agent_state() -> Box<dyn Extension> {
    Box::new(maho_ext_herdr_agent_state::HerdrAgentState)
}
fn orca_agent_status() -> Box<dyn Extension> {
    Box::new(maho_ext_orca_agent_status::OrcaAgentStatus)
}
fn orca_titlebar_spinner() -> Box<dyn Extension> {
    Box::new(maho_ext_orca_titlebar_spinner::OrcaTitlebarSpinner)
}
fn pi_ast_grep() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_ast_grep::AstGrep)
}
fn pi_nested_agents_md() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_nested_agents_md::NestedAgentsMd)
}

/// Linked user/omo extension ports (`crates/extensions/*`) whose crate publishes the constructor.
static USER_FACTORIES: [NativeExtension; 6] = [
    NativeExtension { id: "pi-ast-grep", crate_name: "maho-ext-pi-ast-grep", factory: pi_ast_grep },
    NativeExtension { id: "pi-nested-agents-md", crate_name: "maho-ext-pi-nested-agents-md", factory: pi_nested_agents_md },
    NativeExtension { id: "ferryx-agent-state", crate_name: "maho-ext-ferryx-agent-state", factory: ferryx_agent_state },
    NativeExtension { id: "herdr-agent-state", crate_name: "maho-ext-herdr-agent-state", factory: herdr_agent_state },
    NativeExtension { id: "orca-agent-status", crate_name: "maho-ext-orca-agent-status", factory: orca_agent_status },
    NativeExtension { id: "orca-titlebar-spinner", crate_name: "maho-ext-orca-titlebar-spinner", factory: orca_titlebar_spinner },
];

pub fn user_extensions() -> &'static [NativeExtension] {
    &USER_FACTORIES
}

/// User-port ids whose crate publishes no constructible `Extension` in this worktree.
static DEFERRED_USER_EXTENSIONS: [DeferredExtension; 7] = [
    DeferredExtension { id: "pi-webfetch", crate_name: "maho-ext-pi-webfetch", requirement: "crate `WebfetchExtension` exists only in the `resume-webfetch` worktree (owner: todo 39 pi-webfetch); this worktree's copy publishes the `webfetch` module only" },
    DeferredExtension { id: "omo-zcode-oauth", crate_name: "maho-ext-omo-zcode-oauth", requirement: "`ZcodeOAuth` lives in lane-40 (owner: todo 40); this worktree's `src/lib.rs` is empty" },
    DeferredExtension { id: "pi-rules", crate_name: "maho-ext-pi-rules", requirement: "crate publishes config/commands/index modules; needs the `Extension` factory (owner: todo 39)" },
    DeferredExtension { id: "pi-goal", crate_name: "maho-ext-pi-goal", requirement: "crate publishes index + the goal module; needs the `Extension` factory (owner: todo 39)" },
    DeferredExtension { id: "pi-websearch", crate_name: "maho-ext-pi-websearch", requirement: "crate publishes index + the websearch module; needs the `Extension` factory (owner: todo 39)" },
    DeferredExtension { id: "pi-comment-checker", crate_name: "maho-ext-pi-comment-checker", requirement: "`CommentChecker { source_path: PathBuf }` needs the checker source path the host resolves at startup (owner: todo 40)" },
    DeferredExtension { id: "orca-prefill", crate_name: "maho-ext-orca-prefill", requirement: "`OrcaPrefill { environment: Arc<dyn PrefillEnvironment> }` needs the orca pane environment provider (owner: todo 40)" },
];

pub fn deferred_user_extensions() -> &'static [DeferredExtension] {
    &DEFERRED_USER_EXTENSIONS
}

fn source_info(id: &str, kind: &str) -> SourceInfo {
    SourceInfo { path: format!("<{kind}:{id}>"), source: kind.to_owned(), ..SourceInfo::default() }
}

/// Every linked constructor the CLI can register, builtin first then user ports, in pinned order.
pub fn assemble_extensions() -> Vec<(&'static str, &'static str, Box<dyn Extension>)> {
    BUILTIN_FACTORIES
        .iter()
        .map(|entry| (entry.id, "builtin", (entry.factory)()))
        .chain(USER_FACTORIES.iter().map(|entry| (entry.id, "user", (entry.factory)())))
        .collect()
}

/// The loader factories for [`assemble_extensions`].
pub fn native_extension_factories() -> Vec<NativeExtensionFactory> {
    assemble_extensions()
        .into_iter()
        .map(|(id, kind, extension)| NativeExtensionFactory {
            path: format!("<{kind}:{id}>"),
            source_info: source_info(id, kind),
            extension,
        })
        .collect()
}

/// Register every linked native extension for `cwd`; a failing factory is isolated by the loader.
pub fn load_native_extensions(cwd: &Path, profile: ExtensionSessionProfile) -> LoadExtensionsResult {
    load_extensions(native_extension_factories(), cwd, profile)
}
