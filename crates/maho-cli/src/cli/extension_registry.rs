//! Canonical native extension assembly for the `mhc` binary.
//!
//! Ports senpi `src/extensions/index.ts` (`builtInExtensions`) and
//! `src/core/extensions/builtin/index.ts` (`globalDefaultExtensionIds`, the ordered
//! `builtinExtensions` list). Search routing runs before provider payload injection; every id is
//! bound to the real constructor of its owning crate when that constructor exists without host
//! state. Ids whose constructor needs per-session host state are listed in
//! [`deferred_builtin_extensions`] / [`deferred_user_extensions`] with the exact contract.
//!
//! Registration runs through `maho_ext_host::loader::load_extensions`, the same
//! `register` -> `commit_registration` path a disk-loaded factory takes, with failure isolation per
//! factory.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use maho_ext_api::{Extension, ExtensionSessionProfile, SourceInfo};
use maho_ext_host::loader::{LoadExtensionsResult, NativeExtensionFactory, load_extensions};
use maho_ext_llama::llama;

/// senpi `globalDefaultExtensionIds` (`core/extensions/builtin/index.ts`).
pub const GLOBAL_DEFAULT_EXTENSION_IDS: [&str; 4] = ["diff", "files", "prompt-url-widget", "tps"];

/// Builtin order, with search fallback routing before provider payload injection.
pub const BUILTIN_EXTENSION_IDS: [&str; 45] = [
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
    "websearch",
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
    "webfetch",
    "video-in",
    "look-at",
    "nested-agents-md",
    "rules",
    "goal",
    "loop",
    "cache-keepalive",
    "handoff-hint",
    "relocate-session",
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
    Box::new(maho_ext_model_fallback::ModelFallback {
        is_using_oauth: Arc::new(|model| {
            maho_core::auth_storage::AuthStorage::create_default()
                .get(&model.provider)
                .as_ref()
                .and_then(maho_core::auth_storage::credential_kind)
                == Some(maho_core::auth_storage::CredentialKind::Oauth)
        }),
    })
}
fn relocate_session() -> Box<dyn Extension> {
    Box::new(maho_ext_relocate_session::RelocateSession)
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
    Box::new(maho_ext_help::Help {
        display: Arc::new(|ctx, markdown| {
            Box::pin(async move {
                ctx.ui
                    .select(&markdown, &["Close".into()], Default::default())
                    .await;
                Ok(())
            })
        }),
    })
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
    let reference: maho_ext_goal::accounting_hooks::GoalStoreReference =
        Arc::new(|context: &maho_ext_api::ExtensionContext| {
            maho_ext_goal::store_ref::context_goal_store_ref(context)
        });
    Box::new(maho_ext_goal::GoalExtension::new(reference))
}

/// `loop` binds its store through the owning crate's context helper
/// (`maho_ext_loop::store::loop_store_ref_from_context`); the closure returns the concrete ref the
/// controller requires, falling back to the helper's own no-session namespace only when a persisted
/// session's directory is unresolved.
fn loop_extension() -> Box<dyn Extension> {
    let reference: maho_ext_loop::controller::LoopStoreReference =
        Arc::new(|context: &maho_ext_api::ExtensionContext| {
            maho_ext_loop::store::loop_store_ref_from_context(context).unwrap_or_else(|| {
                maho_ext_loop::types::LoopStoreRef {
                    base_dir: std::path::PathBuf::from(maho_core::config::get_agent_dir())
                        .join("extensions")
                        .join("loop")
                        .join("no-session"),
                    session_id: context.session_manager.session_id().to_owned(),
                }
            })
        });
    Box::new(maho_ext_loop::LoopExtension::new(reference))
}

/// `websearch` takes its config root and the provider-native bypass probe.
fn websearch() -> Box<dyn Extension> {
    let home = std::path::PathBuf::from(maho_core::config::home_dir());
    let provider_native_bypass: maho_ext_websearch::index::ProviderNativeBypass =
        Arc::new(|model| {
            if maho_ext_anthropic_web_search::supports_native_anthropic_web_search(model) {
                return maho_ext_anthropic_web_search::parse_enabled(
                    std::env::var("PI_ANTHROPIC_WEB_SEARCH").ok().as_deref(),
                );
            }
            maho_ext_openai_web_search::supports_native_openai_web_search(model)
                && maho_ext_openai_web_search::parse_enabled(
                    std::env::var("PI_OPENAI_WEB_SEARCH").ok().as_deref(),
                )
        });
    Box::new(maho_ext_websearch::WebsearchExtension {
        home,
        provider_native_bypass,
    })
}

// Constructors that need no per-session host state (unit or `Default`).
fn ttsr() -> Box<dyn Extension> {
    Box::new(maho_ext_ttsr::TtsrExtension)
}
fn btw() -> Box<dyn Extension> {
    Box::new(maho_ext_btw::Btw::default())
}
fn account() -> Box<dyn Extension> {
    Box::new(maho_ext_account::Account)
}
fn config_reload() -> Box<dyn Extension> {
    Box::new(maho_ext_config_reload::ConfigReload::default())
}

/// Linked builtin factories, in the pinned `builtinExtensions` relative order.
static BUILTIN_FACTORIES: [NativeExtension; 27] = [
    NativeExtension {
        id: "loop-guard",
        crate_name: "maho-ext-loop-guard",
        factory: loop_guard,
    },
    NativeExtension {
        id: "hooks",
        crate_name: "maho-ext-hooks",
        factory: hooks,
    },
    NativeExtension {
        id: "permission-system",
        crate_name: "maho-ext-permission-system",
        factory: permission_system,
    },
    NativeExtension {
        id: "herdr",
        crate_name: "maho-ext-herdr",
        factory: herdr,
    },
    NativeExtension {
        id: "prompt-preset",
        crate_name: "maho-ext-prompt-preset",
        factory: prompt_preset,
    },
    NativeExtension {
        id: "websearch",
        crate_name: "maho-ext-websearch",
        factory: websearch,
    },
    NativeExtension {
        id: "anthropic-web-search",
        crate_name: "maho-ext-anthropic-web-search",
        factory: anthropic_web_search,
    },
    NativeExtension {
        id: "anthropic-bash",
        crate_name: "maho-ext-anthropic-bash",
        factory: anthropic_bash,
    },
    NativeExtension {
        id: "openai-web-search",
        crate_name: "maho-ext-openai-web-search",
        factory: openai_web_search,
    },
    NativeExtension {
        id: "model-fallback",
        crate_name: "maho-ext-model-fallback",
        factory: model_fallback,
    },
    NativeExtension {
        id: "recommended-models",
        crate_name: "maho-ext-recommended-models",
        factory: recommended_models,
    },
    NativeExtension {
        id: "bash-timeout",
        crate_name: "maho-ext-bash-timeout",
        factory: bash_timeout,
    },
    NativeExtension {
        id: "terminal",
        crate_name: "maho-ext-terminal",
        factory: terminal,
    },
    NativeExtension {
        id: "tool-pair-guard",
        crate_name: "maho-ext-tool-pair-guard",
        factory: tool_pair_guard,
    },
    NativeExtension {
        id: "compaction",
        crate_name: "maho-ext-compaction",
        factory: compaction,
    },
    NativeExtension {
        id: "help",
        crate_name: "maho-ext-help",
        factory: help,
    },
    NativeExtension {
        id: "webfetch",
        crate_name: "maho-ext-webfetch",
        factory: webfetch,
    },
    NativeExtension {
        id: "video-in",
        crate_name: "maho-ext-video-in",
        factory: video_in,
    },
    NativeExtension {
        id: "nested-agents-md",
        crate_name: "maho-ext-nested-agents-md",
        factory: nested_agents_md,
    },
    NativeExtension {
        id: "rules",
        crate_name: "maho-ext-rules",
        factory: rules,
    },
    NativeExtension {
        id: "goal",
        crate_name: "maho-ext-goal",
        factory: goal,
    },
    NativeExtension {
        id: "loop",
        crate_name: "maho-ext-loop",
        factory: loop_extension,
    },
    NativeExtension {
        id: "relocate-session",
        crate_name: "maho-ext-relocate-session",
        factory: relocate_session,
    },
    NativeExtension {
        id: "ttsr",
        crate_name: "maho-ext-ttsr",
        factory: ttsr,
    },
    NativeExtension {
        id: "btw",
        crate_name: "maho-ext-btw",
        factory: btw,
    },
    NativeExtension {
        id: "account",
        crate_name: "maho-ext-account",
        factory: account,
    },
    NativeExtension {
        id: "config-reload",
        crate_name: "maho-ext-config-reload",
        factory: config_reload,
    },
];

pub fn builtin_extensions() -> &'static [NativeExtension] {
    &BUILTIN_FACTORIES
}

/// Pinned builtin ids this registry cannot construct without host state. Every runtime, and
/// `--list-models`, mounts them through `default_extensions::assembled_factories`; the requirement
/// names the real constructor site.
static DEFERRED_BUILTINS: [DeferredExtension; 18] = [
    DeferredExtension {
        id: "ask-user",
        crate_name: "maho-ext-ask-user",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "gpt-apply-patch",
        crate_name: "maho-ext-gpt-apply-patch",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "imagegen",
        crate_name: "maho-ext-imagegen",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "openai-image-gen",
        crate_name: "maho-ext-openai-image-gen",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "todowrite",
        crate_name: "maho-ext-todotools",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "redraws",
        crate_name: "maho-ext-builtin-loose",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "service-tier",
        crate_name: "maho-ext-builtin-loose",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "reasoning",
        crate_name: "maho-ext-reasoning",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "history-search",
        crate_name: "maho-ext-history-search",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "import-repro",
        crate_name: "maho-ext-builtin-loose",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "look-at",
        crate_name: "maho-ext-look-at",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "cache-keepalive",
        crate_name: "maho-ext-cache-keepalive",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "handoff-hint",
        crate_name: "maho-ext-handoff-hint",
        requirement: "constructed by default_extensions::factories with the parent session's settings reader",
    },
    DeferredExtension {
        id: "gpt-account",
        crate_name: "maho-ext-builtin-loose",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "claude-sdk-oauth",
        crate_name: "maho-ext-anthropic-subscription",
        requirement: "constructed by oauth_providers::runtime_oauth_factories with settings, credential store and environment",
    },
    DeferredExtension {
        id: "cursor-cli-oauth",
        crate_name: "maho-ext-cursor-cli-oauth",
        requirement: "constructed by oauth_providers::runtime_oauth_factories with settings, credential store and environment",
    },
    DeferredExtension {
        id: "tool-search",
        crate_name: "maho-ext-tool-search",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
    DeferredExtension {
        id: "mcp",
        crate_name: "maho-ext-mcp",
        requirement: "constructed by default_extensions::factories with per-session host state",
    },
];

pub fn deferred_builtin_extensions() -> &'static [DeferredExtension] {
    &DEFERRED_BUILTINS
}

/// senpi `src/extensions/index.ts` `builtInExtensions`: inline, hidden, registered beside the
/// 43 builtin ids. The factory is the real llama.cpp provider registration.
pub const INLINE_EXTENSION_IDS: [&str; 1] = ["llama.cpp"];

static INLINE_FACTORIES: [NativeExtension; 1] = [NativeExtension {
    id: "llama.cpp",
    crate_name: "maho-ext-llama",
    factory: llama,
}];

pub fn inline_extensions() -> &'static [NativeExtension] {
    &INLINE_FACTORIES
}

/// senpi `src/extensions/index.ts:4` (`{ name: "llama.cpp", ..., hidden: true }`): inline extensions
/// omitted from the startup Extensions list.
static HIDDEN_INLINE_EXTENSION_IDS: [&str; 1] = ["llama.cpp"];

/// senpi resource-loader.ts:1438 `extension.hidden = isNamed && input.hidden`: marks the loaded
/// hidden inline extensions.
pub fn mark_hidden_inline_extensions(extensions: &mut [maho_ext_api::LoadedExtension]) {
    for extension in extensions {
        if HIDDEN_INLINE_EXTENSION_IDS
            .iter()
            .any(|id| extension.identity.path == format!("<inline:{id}>"))
        {
            extension.hidden = true;
        }
    }
}

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
    Box::new(maho_ext_pi_ast_grep::AstGrep {
        package_entry: Some(installed_git_package_entry("code-yeongyu", "pi-ast-grep")),
    })
}
fn pi_nested_agents_md() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_nested_agents_md::NestedAgentsMd)
}
fn pi_rules() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_rules::RulesExtension)
}
fn pi_websearch() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_websearch::WebsearchExtension {
        native_registry: None,
    })
}
fn pi_webfetch() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_webfetch::WebfetchExtension)
}
/// pi-goal `src/index.ts` `goalExtension`: lifecycle, tools and command over `goalStoreRef`, with the
/// hidden continuation sent through `pi.sendMessage` (`goal/lifecycle.ts` `queueHiddenGoalPrompt`).
fn pi_goal() -> Box<dyn Extension> {
    struct PiGoal;
    impl Extension for PiGoal {
        fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
            let handle = maho_ext_api::ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            );
            let resolve: maho_ext_pi_goal::goal::lifecycle::GoalStoreResolver =
                Arc::new(|context: &maho_ext_api::ExtensionContext| {
                    let session_dir = context
                        .session_manager
                        .session_file()
                        .and_then(|_| context.session_manager.get_session_dir());
                    maho_ext_pi_goal::index::goal_store_ref(
                        &context.agent_dir,
                        session_dir.as_deref(),
                        &context.cwd.to_string_lossy(),
                        context.session_manager.session_id(),
                    )
                });
            let send: maho_ext_pi_goal::goal::lifecycle::GoalContinuationSender =
                Arc::new(move |content: String| {
                    handle.send_message(
                        maho_ext_api::CustomMessage {
                            custom_type: "pi-goal-continuation".into(),
                            content: vec![maho_ext_api::ToolContent::text(content)],
                            display: false,
                            details: None,
                        },
                        maho_ext_api::SendMessageOptions {
                            trigger_turn: true,
                            deliver_as: Some(maho_ext_api::DeliverAs::FollowUp),
                        },
                    )
                });
            if let Err(error) = maho_ext_pi_goal::index::register_goal_extension(api, resolve, send)
            {
                std::panic::panic_any(error);
            }
        }
    }
    Box::new(PiGoal)
}
fn omo_zcode_oauth() -> Box<dyn Extension> {
    Box::new(maho_ext_omo_zcode_oauth::ZcodeOAuth)
}
/// Installed package entry for a `git:github.com/<owner>/<repo>` user package; the native port
/// resolves the package's own `node_modules` from here, as the TS entry does via `import.meta.url`.
fn installed_git_package_entry(owner: &str, repo: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(maho_core::config::get_agent_dir())
        .join("git")
        .join("github.com")
        .join(owner)
        .join(repo)
        .join("src")
        .join("index.ts")
}
fn pi_comment_checker() -> Box<dyn Extension> {
    Box::new(maho_ext_pi_comment_checker::CommentChecker {
        source_path: std::env::var_os("PI_COMMENT_CHECKER_SOURCE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| installed_git_package_entry("code-yeongyu", "pi-comment-checker")),
    })
}
fn orca_prefill() -> Box<dyn Extension> {
    struct Environment;
    impl maho_ext_orca_prefill::PrefillEnvironment for Environment {
        fn pane_key(&self) -> Option<String> {
            std::env::var("ORCA_PANE_KEY").ok()
        }
        fn take_prefill(&self) -> Option<String> {
            maho_ext_orca_prefill::take_process_prefill(|| {
                std::env::var(maho_ext_orca_prefill::PREFILL_ENV).ok()
            })
        }
    }
    Box::new(maho_ext_orca_prefill::OrcaPrefill {
        environment: Arc::new(Environment),
    })
}

/// Linked user/omo extension ports (`crates/extensions/*`) whose crate publishes the constructor.
/// `pi-comment-checker` uses the SUL `comment-checker-core` helpers with `omo` and the MIT
/// `maho-comment-checker` crate without it.
const USER_FACTORY_COUNT: usize = 13;
static USER_FACTORIES: [NativeExtension; USER_FACTORY_COUNT] = [
    NativeExtension {
        id: "pi-ast-grep",
        crate_name: "maho-ext-pi-ast-grep",
        factory: pi_ast_grep,
    },
    NativeExtension {
        id: "pi-nested-agents-md",
        crate_name: "maho-ext-pi-nested-agents-md",
        factory: pi_nested_agents_md,
    },
    NativeExtension {
        id: "pi-rules",
        crate_name: "maho-ext-pi-rules",
        factory: pi_rules,
    },
    NativeExtension {
        id: "pi-websearch",
        crate_name: "maho-ext-pi-websearch",
        factory: pi_websearch,
    },
    NativeExtension {
        id: "pi-webfetch",
        crate_name: "maho-ext-pi-webfetch",
        factory: pi_webfetch,
    },
    NativeExtension {
        id: "pi-goal",
        crate_name: "maho-ext-pi-goal",
        factory: pi_goal,
    },
    NativeExtension {
        id: "ferryx-agent-state",
        crate_name: "maho-ext-ferryx-agent-state",
        factory: ferryx_agent_state,
    },
    NativeExtension {
        id: "herdr-agent-state",
        crate_name: "maho-ext-herdr-agent-state",
        factory: herdr_agent_state,
    },
    NativeExtension {
        id: "orca-agent-status",
        crate_name: "maho-ext-orca-agent-status",
        factory: orca_agent_status,
    },
    NativeExtension {
        id: "orca-titlebar-spinner",
        crate_name: "maho-ext-orca-titlebar-spinner",
        factory: orca_titlebar_spinner,
    },
    NativeExtension {
        id: "omo-zcode-oauth",
        crate_name: "maho-ext-omo-zcode-oauth",
        factory: omo_zcode_oauth,
    },
    NativeExtension {
        id: "pi-comment-checker",
        crate_name: "maho-ext-pi-comment-checker",
        factory: pi_comment_checker,
    },
    NativeExtension {
        id: "orca-prefill",
        crate_name: "maho-ext-orca-prefill",
        factory: orca_prefill,
    },
];

pub fn user_extensions() -> &'static [NativeExtension] {
    &USER_FACTORIES
}

/// User-port ids whose crate publishes no constructible `Extension` in this worktree.
static DEFERRED_USER_EXTENSIONS: [DeferredExtension; 0] = [];

pub fn deferred_user_extensions() -> &'static [DeferredExtension] {
    &DEFERRED_USER_EXTENSIONS
}

/// senpi `getDefaultSourceInfoForPath` (resource-loader.ts:1146-1153): `<builtin:...>` is system
/// scope, every other synthetic `<kind:...>` path is temporary.
fn source_info(id: &str, kind: &str) -> SourceInfo {
    let path = format!("<{kind}:{id}>");
    if kind == "builtin" {
        return super::default_extensions::builtin_source_info(&path);
    }
    SourceInfo {
        path,
        source: kind.to_owned(),
        origin: maho_ext_api::SourceOrigin::TopLevel,
        ..SourceInfo::default()
    }
}

/// Every linked constructor the CLI can register, builtin first then user ports, in pinned order.
pub fn assemble_extensions() -> Vec<(&'static str, &'static str, Box<dyn Extension>)> {
    BUILTIN_FACTORIES
        .iter()
        .map(|entry| (entry.id, "builtin", (entry.factory)()))
        .chain(
            USER_FACTORIES
                .iter()
                .map(|entry| (entry.id, "user", (entry.factory)())),
        )
        .chain(
            INLINE_FACTORIES
                .iter()
                .map(|entry| (entry.id, "inline", (entry.factory)())),
        )
        .collect()
}

/// Package name a settings `packages` source installs (`git:host/owner/<name>[@ref]`,
/// `npm:[@scope/]<name>[@version]`, a local path); the last path segment without ref or `.git`.
fn package_name(source: &str) -> Option<&str> {
    let last = source.trim_end_matches('/').rsplit(['/', ':']).next()?;
    let last = last.strip_suffix(".git").unwrap_or(last);
    last.split('@').next().filter(|name| !name.is_empty())
}

fn configured_package_names(agent_dir: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(agent_dir.join("settings.json")) else {
        return BTreeSet::new();
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BTreeSet::new();
    };
    settings
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|package| {
            package
                .as_str()
                .or_else(|| package.get("source").and_then(serde_json::Value::as_str))
        })
        .filter_map(package_name)
        .map(str::to_owned)
        .collect()
}

/// senpi loads a user extension only when it is installed: a file (or `<id>/index.*` directory) in
/// `<agentDir>/extensions`, or a package listed in the global settings `packages`. A linked native
/// port is registered only for the ids this returns, so uninstalling or deleting one disables it.
pub fn installed_user_extension_ids(agent_dir: &Path) -> BTreeSet<&'static str> {
    let packages = configured_package_names(agent_dir);
    let extensions = agent_dir.join("extensions");
    USER_FACTORIES
        .iter()
        .map(|entry| entry.id)
        .filter(|id| {
            packages.contains(*id)
                || ["ts", "js"].iter().any(|extension| {
                    extensions.join(format!("{id}.{extension}")).is_file()
                        || extensions
                            .join(id)
                            .join(format!("index.{extension}"))
                            .is_file()
                })
        })
        .collect()
}

/// The loader factories for [`assemble_extensions`], with user ports gated on installation in the
/// process agent directory.
pub fn native_extension_factories() -> Vec<NativeExtensionFactory> {
    native_extension_factories_for(Path::new(&maho_core::config::get_agent_dir()))
}

/// [`native_extension_factories`] against an explicit agent directory.
pub fn native_extension_factories_for(agent_dir: &Path) -> Vec<NativeExtensionFactory> {
    let installed = installed_user_extension_ids(agent_dir);
    BUILTIN_FACTORIES
        .iter()
        .map(|entry| (entry.id, "builtin", entry))
        .chain(
            USER_FACTORIES
                .iter()
                .filter(|entry| installed.contains(entry.id))
                .map(|entry| (entry.id, "user", entry)),
        )
        .chain(
            INLINE_FACTORIES
                .iter()
                .map(|entry| (entry.id, "inline", entry)),
        )
        .map(|(id, kind, entry)| NativeExtensionFactory {
            path: format!("<{kind}:{id}>"),
            source_info: source_info(id, kind),
            extension: (entry.factory)(),
        })
        .collect()
}

/// Register every linked native extension for `cwd`; a failing factory is isolated by the loader.
pub fn load_native_extensions(
    cwd: &Path,
    profile: ExtensionSessionProfile,
) -> LoadExtensionsResult {
    load_extensions(native_extension_factories(), cwd, profile)
}
