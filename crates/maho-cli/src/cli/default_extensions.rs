//! Concrete default factories shared by print, RPC and interactive startup.
use maho_ext_api::{Extension, ExtensionActions, ExtensionFailure};
use maho_ext_host::loader::{NativeAsyncExtensionFactory, NativeExtensionFactory};
use std::{rc::Rc, sync::Arc};

pub fn configure_rpc_html_theme_with_paths(
    session: &maho_core::agent_session::AgentSession,
    theme_paths: Vec<std::path::PathBuf>,
) {
    let agent_dir = std::path::PathBuf::from(session.agent_dir());
    let (registered, diagnostics) = super::startup_ui::load_theme_resources(
        theme_paths,
        maho_interactive::theme::detect_color_mode(),
    );
    for diagnostic in diagnostics {
        maho_core::output_guard::maho_write_stderr(&format!("{}\n", diagnostic.message));
    }
    let themes_dir = agent_dir.join("themes");
    let config_theme = session
        .with_settings_manager(|manager| manager.get_string("theme"))
        .unwrap_or_else(|| "dark".into());
    let colorfgbg = std::env::var("COLORFGBG").ok();
    let session_ref = session.weak_accessor();
    // senpi builds the export renderer with `getToolDefinition`, whose built-in `grep`/`find`
    // definitions carry their renderers.
    session.set_html_built_in_tool_renderers(Arc::new(|name: &str| {
        maho_interactive::tools::renderers::built_in_export_renderers(name)
    }));
    session.set_html_theme_resolver(Arc::new(move |requested| {
        let terminal_theme =
            maho_interactive::theme::detection::detect_terminal_background_from_env(
                colorfgbg.as_deref(),
            )
            .theme;
        let mut registry = maho_interactive::theme::registry::ThemeRegistry::new(
            themes_dir.clone(),
            "dark",
            maho_interactive::theme::detect_color_mode(),
        )
        .map_err(|error| error.to_string())?;
        // senpi `extendResources`: themes an extension discovered join the registered set.
        let discovered = session_ref()
            .map(|session| session.discovered_theme_paths())
            .unwrap_or_default();
        let (discovered, _) = super::startup_ui::load_theme_resources(
            discovered.into_iter().map(std::path::PathBuf::from),
            maho_interactive::theme::detect_color_mode(),
        );
        let mut themes = registered.clone();
        for theme in discovered {
            if !themes.iter().any(|known| known.name == theme.name) {
                themes.push(theme);
            }
        }
        registry
            .set_registered_themes(themes)
            .map_err(|error| error.to_string())?;
        // senpi `exportToHtml`: the first of the requested and configured themes that resolves;
        // when neither does, the export falls back to the default (terminal background) theme.
        let theme = [requested, Some(config_theme.as_str())]
            .into_iter()
            .flatten()
            .filter_map(|setting| {
                maho_interactive::theme::resolve_theme_setting(Some(setting), terminal_theme)
            })
            .find_map(|name| registry.load_theme(name).ok());
        let theme = match theme {
            Some(theme) => theme,
            None => registry
                .load_theme(terminal_theme.name())
                .map_err(|error| error.to_string())?,
        };
        let api_theme = maho_interactive::interactive_extension_ui::extension_theme(&theme);
        Ok((
            api_theme,
            Some((theme.resolved_colors(), theme.export_colors().clone())),
        ))
    }));
}

/// senpi `getDefaultSourceInfoForPath` (resource-loader.ts:1146-1153) and `createExtension`
/// (extensions/loader.ts:741-745): a `<builtin:...>` extension is a system-scope top-level resource.
pub fn builtin_source_info(path: &str) -> maho_ext_api::SourceInfo {
    maho_ext_api::SourceInfo {
        path: path.to_owned(),
        source: "builtin".into(),
        scope: maho_ext_api::SourceScope::System,
        origin: maho_ext_api::SourceOrigin::TopLevel,
        base_dir: None,
    }
}

fn factory<E: Extension + 'static>(name: &str, extension: E) -> NativeExtensionFactory {
    let path = format!("<builtin:{name}>");
    NativeExtensionFactory {
        source_info: builtin_source_info(&path),
        path,
        extension: Box::new(extension),
    }
}

/// MIT profile: startup config notices (migration + load diagnostics), the config hot-reload watch,
/// the session-start model profile and the LSP tools (`maho-lsp`, linking through the `mhc-lsp`
/// daemon when it sits beside the executable), the subagent delegation tools (`maho-task`),
/// long-term memory (`maho-memory`), the browser/computer-use tools (`maho-browser`) and telemetry
/// (`maho-telemetry`: local usage ledger, opt-in OTLP export) and the read-only git tools
/// (`maho-git`); delegated tasks can run isolated (`maho-isolation`) and as teams with tmux/cmux
/// panes (`maho-team`); skill support (`maho-skills`: bundled root, `disabled_skills`, the `skill`
/// tool, `/skill`), the builtin MCP servers (`maho-mcp-builtins`) and the session gateway
/// (`maho-gateway`: `session_list`/`session_send`, inbox delivery) and project context files
/// (`maho-context-files`: `/init-deep`, session-start suggestion).
pub fn mit_config_factories() -> Vec<NativeExtensionFactory> {
    vec![
        factory("config-notices", maho_config::ext::ConfigNotices::default()),
        factory(
            "config-hot-reload",
            maho_config::ext::ConfigHotReload::default(),
        ),
        factory("model-profile", maho_model_select::ModelProfileExtension),
        factory(
            "lsp",
            maho_lsp::extension::LspExtension::from_env(Some(std::path::PathBuf::from(
                maho_core::config::get_agent_dir(),
            ))),
        ),
        factory("task", super::delegation::extension()),
        factory("memory", super::long_term_memory::extension()),
        factory("browser", super::browser_automation::extension()),
        factory("telemetry", super::usage_stats::extension()),
        factory("git", super::git_tools::extension()),
        factory("skills", super::skill_library::extension()),
        factory("mcp-builtins", super::builtin_mcp::extension()),
        factory("gateway", super::session_gateway::extension()),
        factory("context-files", super::project_context::extension()),
    ]
}

pub fn async_factories(factories: Vec<NativeExtensionFactory>) -> Vec<NativeAsyncExtensionFactory> {
    factories
        .into_iter()
        .map(|factory| {
            let extension: Arc<dyn Extension> = Arc::from(factory.extension);
            NativeAsyncExtensionFactory {
                path: factory.path,
                source_info: factory.source_info,
                factory: Arc::new(move |api| {
                    extension.register(api);
                    Box::pin(async { Ok(()) })
                }),
            }
        })
        .collect()
}

/// senpi `getActiveBuiltinExtensionIds` (`resource-loader.ts`): the settings allowlist
/// (`enabledBuiltinExtensions`) and denylist (`disabledBuiltinExtensions`) over the pinned builtin
/// ids and the bundled `codemode` extension.
pub struct BuiltinExtensionSelection {
    pub enabled: Option<Vec<String>>,
    pub disabled: Vec<String>,
}

impl BuiltinExtensionSelection {
    pub fn from_settings(settings: &maho_core::settings_manager::SettingsManager) -> Self {
        Self {
            enabled: settings.get_enabled_builtin_extensions(),
            disabled: settings.get_disabled_builtin_extensions(),
        }
    }

    /// Whether `id` loads; ids outside senpi's builtin and bundled sets are never filtered.
    pub fn is_active(&self, id: &str) -> bool {
        if !super::extension_registry::BUILTIN_EXTENSION_IDS.contains(&id) && id != "codemode" {
            return true;
        }
        self.enabled
            .as_ref()
            .is_none_or(|enabled| enabled.iter().any(|candidate| candidate == id))
            && !self.disabled.iter().any(|candidate| candidate == id)
    }
}

fn builtin_id(path: &str) -> Option<&str> {
    path.strip_prefix("<builtin:")
        .and_then(|path| path.strip_suffix('>'))
}

/// Every builtin factory a runtime mounts, OAuth provider lanes included, filtered by the settings
/// selection and sorted into senpi's resource-loader order: `builtinExtensions` (MCP last), the bundled
/// and inline factories, then the global defaults senpi loads through `<agentDir>/extensions` shims.
pub fn assembled_factories(
    widget_sender: tokio::sync::mpsc::UnboundedSender<
        maho_interactive::interactive_extension_ui::UiRequest,
    >,
    parent: TaskParent,
    settings: &maho_core::settings_manager::SettingsManager,
    cwd: &std::path::Path,
    agent_dir: &str,
) -> Vec<NativeAsyncExtensionFactory> {
    assembled_factories_with_mcp_registry(widget_sender, parent, None, settings, cwd, agent_dir)
}

/// senpi `resource-loader` `mcpRegistry`: a multi-session host passes its one shared
/// `HostMcpRegistry` so every session leases the same MCP connections.
pub fn assembled_factories_with_mcp_registry(
    widget_sender: tokio::sync::mpsc::UnboundedSender<
        maho_interactive::interactive_extension_ui::UiRequest,
    >,
    parent: TaskParent,
    mcp_registry: Option<Arc<maho_ext_mcp::host_registry::HostMcpRegistry>>,
    settings: &maho_core::settings_manager::SettingsManager,
    cwd: &std::path::Path,
    agent_dir: &str,
) -> Vec<NativeAsyncExtensionFactory> {
    let mut assembled = factories_with_mcp_registry(widget_sender, parent.clone(), mcp_registry);
    // The clean-room config and model-profile extensions (`maho-config`, `maho-model-select`).
    {
        drop(parent);
        assembled.extend(async_factories(mit_config_factories()));
    }
    assembled.extend(super::oauth_providers::runtime_oauth_factories(
        settings, cwd, agent_dir,
    ));
    let agent_dir_path = std::path::Path::new(agent_dir);
    if let Err(error) =
        super::global_default_shims::ensure_global_default_extensions(agent_dir_path)
    {
        maho_core::output_guard::maho_write_stderr(&format!(
            "Could not write default extension shims: {error}\n"
        ));
    }
    let active_defaults =
        super::global_default_shims::active_global_default_extension_ids(agent_dir_path);
    assembled.retain(|factory| {
        let id = builtin_id(&factory.path);
        !id.is_some_and(|id| {
            super::extension_registry::GLOBAL_DEFAULT_EXTENSION_IDS.contains(&id)
                && !active_defaults.contains(id)
        })
    });
    let paths: std::collections::BTreeSet<_> = assembled
        .iter()
        .map(|factory| factory.path.clone())
        .collect();
    let native = super::extension_registry::native_extension_factories_for(agent_dir_path)
        .into_iter()
        .filter(|factory| !paths.contains(&factory.path))
        .collect();
    assembled.extend(async_factories(native));
    // senpi `getHarnessExtensionSourceInfo` (resource-loader.ts:1472-1480): the global defaults load
    // through generated `<agentDir>/extensions` shims, a system-scope builtin based there.
    for factory in &mut assembled {
        if builtin_id(&factory.path)
            .is_some_and(|id| super::extension_registry::GLOBAL_DEFAULT_EXTENSION_IDS.contains(&id))
        {
            factory.source_info.base_dir = Some(
                agent_dir_path
                    .join("extensions")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    let selection = BuiltinExtensionSelection::from_settings(settings);
    assembled.retain(|factory| builtin_id(&factory.path).is_none_or(|id| selection.is_active(id)));
    // senpi `loadFinalExtensionSet` (resource-loader.ts:902-903, :933-949): builtin and inline
    // factories come first, then the path-loaded extensions, where the global defaults load through
    // their generated `<agentDir>/extensions/<id>` shims.
    assembled.sort_by_key(|factory| {
        let id = builtin_id(&factory.path);
        let builtin_count = super::extension_registry::BUILTIN_EXTENSION_IDS.len();
        if id
            .is_some_and(|id| super::extension_registry::GLOBAL_DEFAULT_EXTENSION_IDS.contains(&id))
        {
            builtin_count + 1
        } else {
            id.and_then(|id| {
                super::extension_registry::BUILTIN_EXTENSION_IDS
                    .iter()
                    .position(|candidate| *candidate == id)
            })
            .unwrap_or(builtin_count)
        }
    });
    assembled
}

/// senpi `noExtensions` (resource-loader.ts:701-703): only CLI paths load, so the auto-discovered
/// global default shims and the linked user ports are dropped; builtins and inline factories stay.
pub fn retain_no_extensions_factories(factories: &mut Vec<NativeAsyncExtensionFactory>) {
    factories.retain(|factory| {
        factory.source_info.source != "user"
            && !builtin_id(&factory.path).is_some_and(|id| {
                super::extension_registry::GLOBAL_DEFAULT_EXTENSION_IDS.contains(&id)
            })
    });
}

/// senpi resource-loader order after the builtins: discovered user/settings/`-e` extensions are
/// appended last. `--no-extensions` keeps only the `-e` paths (senpi `noExtensions` skips user and
/// settings paths, never builtins; explicit `-e` entries, including `builtin:<id>` linked entries
/// such as `builtin:task-member`, still load). Extensions that cannot load are returned beside the
/// factories: senpi `collectExtensionLoadDiagnostics` (main.ts:129-136) reports them as warnings and continues.
pub async fn with_user_extensions(
    mut factories: Vec<NativeAsyncExtensionFactory>,
    settings: &mut maho_core::settings_manager::SettingsManager,
    cwd: &str,
    agent_dir: &str,
    cli_extension_paths: &[String],
    no_extensions: bool,
) -> Result<
    (
        Vec<NativeAsyncExtensionFactory>,
        Vec<super::user_extensions::UserExtensionError>,
    ),
    String,
> {
    if no_extensions {
        retain_no_extensions_factories(&mut factories);
    }
    let discovery = super::user_extensions::resolve_and_discover(
        settings,
        cwd,
        agent_dir,
        cli_extension_paths,
        no_extensions,
    )
    .await
    .map_err(|error| error.to_string())?;
    // Discovery applies the settings overrides, package filters and project scope, so it decides
    // which linked user ports load; each registers once, with the discovered provenance.
    factories.retain(|factory| !factory.path.starts_with("<user:"));
    factories.extend(discovery.factories);
    Ok((factories, discovery.errors))
}

/// senpi `collectExtensionLoadDiagnostics` message for one extension that could not load.
pub fn extension_load_failure_message(path: &str, error: &str) -> String {
    format!("Failed to load extension \"{path}\": {error}")
}

/// senpi `with_user_extensions` failures as runner load errors (reload has no startup warning list).
fn user_extension_load_error(
    error: super::user_extensions::UserExtensionError,
) -> maho_ext_api::ExtensionError {
    maho_ext_api::ExtensionError {
        error: extension_load_failure_message(&error.path, &error.message),
        extension_path: error.path,
        event: "load".into(),
        stack: None,
    }
}

/// senpi `AgentSession.reload` -> `_resourceLoader.reload()` (resource-loader.ts:613-708) ->
/// `_buildRuntime`: every extension-runner recreate re-reads settings, re-applies the builtin
/// selection and global-default shims, re-resolves packages and user extensions, then loads the
/// final set. The factory list captured at startup is never reused.
pub fn reloading_runtime_factory(
    widget_sender: tokio::sync::mpsc::UnboundedSender<
        maho_interactive::interactive_extension_ui::UiRequest,
    >,
    parent: TaskParent,
    mcp_registry: Option<Arc<maho_ext_mcp::host_registry::HostMcpRegistry>>,
    cli_extension_paths: Vec<String>,
    no_extensions: bool,
    profile: maho_ext_api::ExtensionSessionProfile,
) -> maho_ext_host::runner::RuntimeFactory {
    Arc::new(move |context| {
        let cwd = context.cwd.to_string_lossy().into_owned();
        let agent_dir = context.agent_dir.to_string_lossy().into_owned();
        let mut settings = maho_core::settings_manager::SettingsManager::create(
            &cwd,
            &agent_dir,
            &maho_core::config::home_dir(),
            (context.is_project_trusted_fn)(),
        );
        let factories = assembled_factories_with_mcp_registry(
            widget_sender.clone(),
            parent.clone(),
            mcp_registry.clone(),
            &settings,
            std::path::Path::new(&cwd),
            &agent_dir,
        );
        let (cli_extension_paths, profile) = (cli_extension_paths.clone(), profile.clone());
        Box::pin(async move {
            let (factories, errors) = with_user_extensions(
                factories,
                &mut settings,
                &cwd,
                &agent_dir,
                &cli_extension_paths,
                no_extensions,
            )
            .await
            .map_err(ExtensionFailure::new)?;
            let mut runner = maho_ext_host::runner::ExtensionRunner::from_async_factories(
                factories, context, profile,
            )
            .await?;
            for error in errors {
                runner.emit_error(user_extension_load_error(error));
            }
            Ok(runner)
        })
    })
}

pub fn factories(
    widget_sender: tokio::sync::mpsc::UnboundedSender<
        maho_interactive::interactive_extension_ui::UiRequest,
    >,
    parent: TaskParent,
) -> Vec<NativeAsyncExtensionFactory> {
    factories_with_mcp_registry(widget_sender, parent, None)
}

pub fn factories_with_mcp_registry(
    widget_sender: tokio::sync::mpsc::UnboundedSender<
        maho_interactive::interactive_extension_ui::UiRequest,
    >,
    parent: TaskParent,
    mcp_registry: Option<Arc<maho_ext_mcp::host_registry::HostMcpRegistry>>,
) -> Vec<NativeAsyncExtensionFactory> {
    let fallback_parent = parent.clone();
    let help_widget_sender = widget_sender.clone();
    let history_widget_sender = widget_sender.clone();
    let file_widget_sender = widget_sender.clone();
    let file_render: maho_ext_builtin_loose::files::RenderBinding = Arc::new(move |_| {
        let sender = file_widget_sender.clone();
        Rc::new(move || {
            drop(sender.send(maho_interactive::interactive_extension_ui::UiRequest::WidgetFrame))
        })
    });
    let mcp_gate: Arc<std::sync::Mutex<Option<maho_ext_mcp::service::McpNativeToolSearchGate>>> =
        Arc::new(std::sync::Mutex::new(None));
    let tool_search_gate = mcp_gate.clone();
    let tool_search_service: Arc<
        std::sync::Mutex<Option<super::tool_search::SharedToolSearchService>>,
    > = Arc::new(std::sync::Mutex::new(None));
    let tool_search_slot = tool_search_service.clone();
    vec![
            #[cfg(windows)]
            factory("memory-report", super::memory_report_registry::MemoryReportCommand),
            factory("recommended-models", maho_ext_recommended_models::RecommendedModels),
            factory("relocate-session", maho_ext_relocate_session::RelocateSession),
            factory("permission-system", maho_ext_permission_system::PermissionSystem),
            factory("gpt-apply-patch", maho_ext_gpt_apply_patch::index::ApplyPatchExtension),
            factory("tool-search", ToolSearch { mcp_native_enabled: Arc::new(move || tool_search_gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|gate| gate.enabled())), service: tool_search_slot }),
            factory("todowrite", Todo(widget_sender)),
            factory("websearch", maho_ext_websearch::index::WebsearchExtension { home: maho_core::config::home_dir().into(),
                provider_native_bypass: Arc::new(|model| {
                    if maho_ext_anthropic_web_search::supports_native_anthropic_web_search(model) {
                        return maho_ext_anthropic_web_search::parse_enabled(std::env::var("PI_ANTHROPIC_WEB_SEARCH").ok().as_deref());
                    }
                    maho_ext_openai_web_search::supports_native_openai_web_search(model)
                        && maho_ext_openai_web_search::parse_enabled(std::env::var("PI_OPENAI_WEB_SEARCH").ok().as_deref())
                }),
            }),
            factory("look-at", maho_ext_look_at::index::LookAtExtension {
                runner: maho_ext_look_at::runner::create_vision_runner(Arc::new(|ctx, model, context, options| ctx.model_registry.stream_simple(model, context, Some(options))),
                    Some(Arc::new(|bytes, mime, options| Box::pin(async move {
                        let prepared = super::super::utils::image_process::process_image(&bytes, &mime,
                            super::super::utils::image_process::ProcessImageOptions { auto_resize_images: Some(options.auto_resize_images), ..Default::default() }).await.map_err(ExtensionFailure::new)?;
                        Ok(maho_ext_look_at::image_input::ProcessedImage { data: prepared.data, mime_type: prepared.mime_type, hints: prepared.hints })
                    })))),
            }),
            factory("account", maho_ext_account::Account),
            factory("service-tier", maho_ext_builtin_loose::service_tier::ServiceTierExtension {
                host: Arc::new(ServiceTierHost),
            }),
            factory("reasoning", maho_ext_reasoning::Reasoning {
                host: Arc::new(super::reasoning_host::NativeReasoningHost::new(Arc::new({
                    let parent = parent.clone();
                    move || parent.get().and_then(|parent| parent())
                }))),
            }),
            factory("import-repro", maho_ext_builtin_loose::import_repro::ImportRepro {
                session_dir: Arc::new(|ctx| ctx.session_manager.get_session_dir().ok_or_else(|| ExtensionFailure::new("Session directory is unavailable"))),
                fetch: maho_ext_builtin_loose::import_repro::github_fetch(),
            }),
            factory("diff", maho_ext_builtin_loose::diff::Diff { render_binding: file_render.clone() }),
            factory("files", maho_ext_builtin_loose::files::Files { render_binding: file_render }),
            factory("prompt-url-widget", maho_ext_builtin_loose::prompt_url_widget::PromptUrlWidget),
            factory("tps", maho_ext_builtin_loose::tps::Tps),
            factory("redraws", maho_ext_builtin_loose::redraws::Redraws { full_redraws: Arc::new(|host| host.full_redraws()) }),
            factory("gpt-account", maho_ext_builtin_loose::gpt_account::GptAccount {
                open_browser: maho_ext_builtin_loose::oauth_login_interaction::platform_open_browser(),
                login: Arc::new({
                    let parent = parent.clone();
                    move |ctx| {
                        let session = parent.get().and_then(|parent| parent());
                        Box::pin(async move {
                            let session = session.ok_or_else(|| ExtensionFailure::new("No live session"))?;
                            super::gpt_account_host::login_account(&ctx, session.model_runtime()).await
                        })
                    }
                }),
            }),
            factory("ask-user", maho_ext_ask_user::AskUser),
            factory("video-in", maho_ext_video_in::VideoIn),
            factory("anthropic-web-search", maho_ext_anthropic_web_search::AnthropicWebSearch),
            factory("openai-web-search", maho_ext_openai_web_search::OpenAiWebSearch),
            factory("imagegen", maho_ext_imagegen::ImageGen::default()),
            factory("openai-image-gen", maho_ext_openai_image_gen::OpenAiImageGen),
            factory("herdr", maho_ext_herdr::Herdr),
            factory("btw", maho_ext_btw::Btw::default()),
            factory("cache-keepalive", maho_ext_cache_keepalive::CacheKeepalive::default()),
            factory("handoff-hint", maho_ext_handoff_hint::HandoffHint {
                settings: Arc::new({
                    let parent = parent.clone();
                    move || {
                        parent.get().and_then(|parent| parent())
                            .map(|session| session.with_settings_manager(|manager| manager.get_handoff_hint_settings()))
                            .map_or(maho_ext_handoff_hint::DEFAULT_HANDOFF_HINT_SETTINGS, |resolved| maho_ext_handoff_hint::HandoffHintSettings {
                                enabled: resolved.enabled, min_context_tokens: resolved.min_context_tokens, min_saving_usd: resolved.min_saving_usd,
                            })
                    }
                }),
                ..Default::default()
            }),
            factory("help", maho_ext_help::Help { display: maho_ext_help::native_display(maho_ext_help::HelpUiBindings {
                render: {
                    let render_sender = help_widget_sender.clone();
                    Arc::new(move |_| {
                        let widget_sender = render_sender.clone();
                        Rc::new(move || drop(widget_sender.send(maho_interactive::interactive_extension_ui::UiRequest::WidgetFrame)))
                    })
                },
                rows: Arc::new(|host: &dyn maho_ext_api::ExtensionTuiHost| {
                    let dimensions = host.dimensions_reader();
                    Rc::new(move || usize::from(dimensions().1))
                }),
                theme: Arc::new(|theme| {
                    maho_interactive::theme::Theme::builtin(
                        theme.name.as_deref().unwrap_or("dark"),
                        maho_interactive::theme::detect_color_mode(),
                    ).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
                }),
            }) }),
            factory("history-search", maho_ext_history_search::HistorySearch {
                session_dir: Arc::new(|ctx| ctx.session_manager.get_session_dir().ok_or_else(|| ExtensionFailure::new("History search requires a session directory"))),
                default_sessions_root: Arc::new(|| std::path::PathBuf::from(maho_core::config::get_agent_dir()).join("sessions")),
                select: maho_ext_history_search::native_selection(maho_ext_history_search::HistoryUiBindings {
                    render: Arc::new(move |_| {
                        let widget_sender = history_widget_sender.clone();
                        Rc::new(move || drop(widget_sender.send(maho_interactive::interactive_extension_ui::UiRequest::WidgetFrame)))
                    }),
                    theme: Arc::new(|theme| {
                        maho_interactive::theme::Theme::builtin(
                            theme.name.as_deref().unwrap_or("dark"),
                            maho_interactive::theme::detect_color_mode(),
                        ).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
                    }),
                }),
            }),
            factory("model-fallback", maho_ext_model_fallback::ModelFallback { is_using_oauth: Arc::new(move |model| {
                fallback_parent.get().and_then(|parent| parent()).is_some_and(|parent| parent.model_registry().is_using_oauth(model))
            }) }),
            factory("compaction", maho_ext_compaction::CompactionExtension),
            factory("webfetch", maho_ext_webfetch::index::WebfetchExtension),
            factory("rules", maho_ext_rules::Rules),
            factory("goal", maho_ext_goal::GoalExtension::default()),
            factory("codemode", Codemode { parent: parent.clone() }),
            // RESERVED (assembly, sequential): the builtin-mcps owner's native MCP declaration is
            // added here once its source receipt lands; this is the MCP declaration seam the plan
            // marks integrator-owned. Do not integrate from a summary.
            factory("mcp", Mcp { gate: mcp_gate.clone(), tool_search: tool_search_service.clone(), shared_registry: mcp_registry }),
        ].into_iter().map(|factory| {
            let extension: Arc<dyn Extension> = Arc::from(factory.extension);
            NativeAsyncExtensionFactory { path: factory.path, source_info: factory.source_info,
                factory: Arc::new(move |api| { extension.register(api); Box::pin(async { Ok(()) }) }) }
        }).collect()
}

#[doc(hidden)]
pub struct ServiceTierHost;
impl maho_ext_builtin_loose::service_tier::ServiceTierHost for ServiceTierHost {
    fn catalog_tier(&self, model: &maho_ext_api::Model) -> Option<maho_ext_api::ServiceTier> {
        model.service_tier.map(|tier| match tier {
            maho_ai::types::ServiceTierPreference::Auto => maho_ext_api::ServiceTier::Auto,
            maho_ai::types::ServiceTierPreference::Flex => maho_ext_api::ServiceTier::Flex,
            maho_ai::types::ServiceTierPreference::Priority => maho_ext_api::ServiceTier::Priority,
            maho_ai::types::ServiceTierPreference::Ultrafast => {
                maho_ext_api::ServiceTier::Ultrafast
            }
        })
    }
    fn upstream_id(&self, model: &maho_ext_api::Model) -> String {
        model
            .upstream_model_id
            .clone()
            .unwrap_or_else(|| model.id.clone())
    }
    fn remembered(
        &self,
        ctx: &maho_ext_api::ExtensionContext,
        model: &maho_ext_api::Model,
    ) -> Result<Option<maho_ext_api::ServiceTier>, ExtensionFailure> {
        let manager = maho_core::settings_manager::SettingsManager::create(
            &ctx.cwd.to_string_lossy(),
            &ctx.agent_dir.to_string_lossy(),
            &maho_core::config::home_dir(),
            (ctx.is_project_trusted_fn)(),
        );
        Ok(manager
            .get_model_service_tier(&model.provider, &model.id)
            .and_then(parse_service_tier))
    }
    fn global_tier(
        &self,
        ctx: &maho_ext_api::ExtensionContext,
    ) -> Result<Option<maho_ext_api::ServiceTier>, ExtensionFailure> {
        let manager = maho_core::settings_manager::SettingsManager::create(
            &ctx.cwd.to_string_lossy(),
            &ctx.agent_dir.to_string_lossy(),
            &maho_core::config::home_dir(),
            (ctx.is_project_trusted_fn)(),
        );
        Ok(manager
            .get_string("openai.serviceTier")
            .as_deref()
            .and_then(parse_service_tier))
    }
    fn persist<'a>(
        &'a self,
        ctx: &'a maho_ext_api::ExtensionContext,
        model: &'a maho_ext_api::Model,
        tier: maho_ext_api::ServiceTier,
    ) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let mut manager = maho_core::settings_manager::SettingsManager::create(
                &ctx.cwd.to_string_lossy(),
                &ctx.agent_dir.to_string_lossy(),
                &maho_core::config::home_dir(),
                (ctx.is_project_trusted_fn)(),
            );
            let tier = match tier {
                maho_ext_api::ServiceTier::Auto => "auto",
                maho_ext_api::ServiceTier::Flex => "flex",
                maho_ext_api::ServiceTier::Priority => "priority",
                maho_ext_api::ServiceTier::Ultrafast => "ultrafast",
            };
            manager
                .set_model_service_tier(&model.provider, &model.id, tier)
                .map_err(ExtensionFailure::new)
        })
    }
}
fn parse_service_tier(tier: &str) -> Option<maho_ext_api::ServiceTier> {
    match tier {
        "auto" => Some(maho_ext_api::ServiceTier::Auto),
        "flex" => Some(maho_ext_api::ServiceTier::Flex),
        "priority" => Some(maho_ext_api::ServiceTier::Priority),
        "ultrafast" => Some(maho_ext_api::ServiceTier::Ultrafast),
        _ => None,
    }
}

pub fn test_service_tier_host() -> impl maho_ext_builtin_loose::service_tier::ServiceTierHost {
    ServiceTierHost
}

struct RuntimeActions(maho_ext_api::ExtensionRuntime);
pub type TaskParent = Arc<
    std::sync::OnceLock<
        Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
    >,
>;

struct Codemode {
    parent: TaskParent,
}
struct Images;
impl maho_codemode::tool::image_resize::EvalImageSdk for Images {
    fn resize_image<'a>(
        &'a self,
        bytes: Vec<u8>,
        mime: &'a str,
        max_bytes: Option<usize>,
    ) -> maho_codemode::tool::image_resize::ImageFuture<
        'a,
        Option<maho_codemode::tool::image_resize::ResizedImage>,
    > {
        Box::pin(async move {
            // senpi `resizeEvalImage`: `resizeImage()` returning null keeps the original image with
            // no note, and the only note is `formatDimensionNote(resized)`.
            let mut resize = super::super::utils::image_resize::ImageResizeOptions::default();
            if let Some(max_bytes) = max_bytes {
                resize.max_bytes = max_bytes as f64;
            }
            let Some(resized) =
                super::super::utils::image_resize::resize_image(&bytes, mime, resize).await
            else {
                return Ok(None);
            };
            let dimension_note = super::super::utils::image_resize::format_dimension_note(&resized)
                .unwrap_or_default();
            Ok(Some(maho_codemode::tool::image_resize::ResizedImage {
                data: resized.data,
                mime_type: resized.mime_type,
                dimension_note,
            }))
        })
    }
    fn convert_to_png<'a>(
        &'a self,
        data: &'a str,
        mime_type: &'a str,
    ) -> maho_codemode::tool::image_resize::ImageFuture<
        'a,
        Option<maho_codemode::tool::image_resize::EvalImageContent>,
    > {
        Box::pin(async move {
            Ok(
                super::super::utils::image_convert::convert_to_png(data, mime_type).map(|image| {
                    maho_codemode::tool::image_resize::EvalImageContent {
                        data: image.data,
                        mime_type: image.mime_type,
                    }
                }),
            )
        })
    }
}
impl Extension for Codemode {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let env_passthrough = self.parent.get().and_then(|p| p()).map(|s| s.with_settings_manager(|m| m.get_bash_env_scrub_passthrough())).unwrap_or_else(|| maho_core::settings_manager::SettingsManager::create(&api.registered.registration_cwd.to_string_lossy(), &maho_core::config::get_agent_dir(), &maho_core::config::home_dir(), true).get_bash_env_scrub_passthrough());
        if let Err(error) = maho_codemode::register(
            api,
            maho_codemode::CodemodeExtensionOptions {
                image_sdk: Arc::new(Images),
                complete: Arc::new(|request, context| {
                    Box::pin(super::codemode_services::complete(request, context))
                }),
                home_dir: maho_core::config::home_dir().into(),
                environment: std::env::vars().collect(),
                js_runtime: maho_codemode::extension::runtime_info::probe_js_runtime_info(
                    &std::env::vars().collect(),
                ),
                env_passthrough,
            },
        ) {
            std::panic::panic_any(error);
        }
    }
}
impl RuntimeActions {
    fn actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.0.message_actions()
    }
}
impl ExtensionActions for RuntimeActions {
    fn send_message(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.actions()?.send_message(message, options)
    }
    fn send_message_with_receipt(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<maho_ext_api::MessageDeliveryReceipt, ExtensionFailure> {
        self.actions()?.send_message_with_receipt(message, options)
    }
    fn send_user_message(
        &self,
        content: maho_ext_api::UserMessageContent,
        options: maho_ext_api::SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.actions()?.send_user_message(content, options)
    }
    fn append_entry(
        &self,
        kind: &str,
        data: Option<maho_ext_api::JsonValue>,
    ) -> Result<(), ExtensionFailure> {
        self.actions()?.append_entry(kind, data)
    }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, ExtensionFailure> {
        self.actions()?.get_all_tools()
    }
}
struct ToolSearch {
    mcp_native_enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    service: Arc<std::sync::Mutex<Option<super::tool_search::SharedToolSearchService>>>,
}
impl Extension for ToolSearch {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        // Create the ONE shared tool-search service and publish it for the MCP factory, so the
        // tool_search tool, the MCP catalog feed, the native gate and the adapter all read it.
        let service = Arc::new(tokio::sync::Mutex::new(
            maho_ext_tool_search::service::ToolSearchService::new(
                api.runtime.clone(),
                Arc::new(RuntimeActions(api.runtime.clone())),
            ),
        ));
        maho_ext_tool_search::index::ToolSearchExtension {
            actions: Arc::new(RuntimeActions(api.runtime.clone())),
            mcp_native_enabled: self.mcp_native_enabled.clone(),
        }
        .register_with_service(api, service.clone());
        *self
            .service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service);
    }
}
struct Mcp {
    gate: Arc<std::sync::Mutex<Option<maho_ext_mcp::service::McpNativeToolSearchGate>>>,
    tool_search: Arc<std::sync::Mutex<Option<super::tool_search::SharedToolSearchService>>>,
    shared_registry: Option<Arc<maho_ext_mcp::host_registry::HostMcpRegistry>>,
}
/// Distinct lease owner per session service on a host-shared registry (senpi passes the
/// `McpService` instance itself as the owner).
static NEXT_MCP_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
impl Extension for Mcp {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let tool_search = self
            .tool_search
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let owner = NEXT_MCP_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (registry, session_owned) = match &self.shared_registry {
            Some(registry) => (registry.clone(), false),
            None => (
                Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default()),
                true,
            ),
        };
        let extension = maho_ext_mcp::index::McpExtension {
            registry: registry.clone(),
            owner,
            tool_search,
        };
        let service = extension.register_with_service(api);
        // A host-shared registry outlives every session: the service's own shutdown detaches this
        // owner's leases, and only a session-private registry is torn down with the session.
        if session_owned {
            api.on(
                maho_ext_api::EventKind::SessionShutdown,
                Arc::new(move |_, _| {
                    let registry = registry.clone();
                    Box::pin(async move {
                        registry.dispose().await.map_err(|error| {
                            maho_ext_api::ExtensionFailure::new(error.to_string())
                        })?;
                        Ok(maho_ext_api::EventResult::None)
                    })
                }),
            );
        }
        if let Ok(service) = service.try_lock() {
            *self
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(service.native_tool_search_gate());
        }
    }
}
struct Todo(
    tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>,
);
impl Extension for Todo {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        maho_ext_todotools::index::NativeTodotoolsExtension {
            actions: Arc::new(RuntimeActions(api.runtime.clone())),
            widget_sender: self.0.clone(),
            copy_markdown: Arc::new(|text| {
                Box::pin(async move {
                    super::super::utils::clipboard::copy_to_clipboard(&text)
                        .await
                        .map_err(ExtensionFailure::new)
                })
            }),
        }
        .register(api);
    }
}

#[cfg(test)]
mod mit_tests {
    #[test]
    fn mit_profile_mounts_the_mit_extensions_in_order() {
        let paths: Vec<String> = super::mit_config_factories()
            .into_iter()
            .map(|factory| factory.path)
            .collect();
        assert_eq!(
            paths,
            [
                "<builtin:config-notices>",
                "<builtin:config-hot-reload>",
                "<builtin:model-profile>",
                "<builtin:lsp>",
                "<builtin:task>",
                "<builtin:memory>",
                "<builtin:browser>",
                "<builtin:telemetry>",
                "<builtin:git>",
                "<builtin:skills>",
                "<builtin:mcp-builtins>",
                "<builtin:gateway>",
                "<builtin:context-files>"
            ]
        );
    }
}
