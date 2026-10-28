//! The `<builtin:side-panel>` extension: flag, session lifecycle wiring, the column component,
//! the viewer overlay and `/side-panel-diff` (spec 1, 3, 6, 7).

use crate::layout::{self, MountMode};
use crate::links::{self, Link};
use crate::panel::{
    ExecError, HostFuture, MemoryReader, Panel, PanelEnv, PanelHost, UsageReader, theme_painter,
};
use crate::sections::{MemoryInfo, SessionFacts};
use crate::settings::{Settings, SettingsError, panel_enabled};
use crate::tasks;
use crate::viewer::{KeyOutcome, Viewer};
use maho_ext_api::{
    CustomUiFactoryOptions, EventKind, EventResult, ExecOptions, ExecResult, Extension,
    ExtensionApi, ExtensionContext, ExtensionEvent, ExtensionFailure, ExtensionMode,
    ExtensionSideColumn, ExtensionUi, ExtensionUiDialogOptions, FlagType, FlagValue,
    NotificationType, Theme,
};
use maho_tui::tui::{
    Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The per-run flag (interface fact).
pub const FLAG_NAME: &str = "omo-side-panel";
/// Neutral alias of [`FLAG_NAME`].
pub const FLAG_ALIAS: &str = "side-panel";
/// The diff command (interface fact).
pub const DIFF_COMMAND: &str = "side-panel-diff";

/// Builds the memory status for a session cwd (`None` = nothing resolves).
pub type MemoryStatus = Arc<dyn Fn(&Path) -> Option<MemoryInfo> + Send + Sync>;

/// Loads the merged user configuration for a cwd: the merged view plus every raw `side_panel`
/// layer value (validated separately so an invalid value is reported, not silently dropped).
pub type ConfigSource = Arc<dyn Fn(&Path) -> LoadedConfig + Send + Sync>;

/// Configuration the panel reads at session start.
#[derive(Clone, Debug, Default)]
pub struct LoadedConfig {
    pub view: Value,
    pub side_panel_layers: Vec<Value>,
}

/// The side panel extension.
#[derive(Clone)]
pub struct SidePanelExtension {
    pub config: ConfigSource,
    /// Memory status source; `None` hides the memory section.
    pub memory: Option<MemoryStatus>,
    /// Plan usage source; `None` hides the usage section.
    pub usage: Option<UsageReader>,
    /// Environment lookup (hyperlink support, `$HOME`).
    pub env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

impl std::fmt::Debug for SidePanelExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidePanelExtension").finish_non_exhaustive()
    }
}

impl Default for SidePanelExtension {
    fn default() -> Self {
        Self {
            config: user_config(),
            memory: None,
            usage: None,
            env: Arc::new(|key: &str| std::env::var(key).ok()),
        }
    }
}

/// Reads `~/.omo/omo.jsonc` and the project config through `maho-config`.
pub fn user_config() -> ConfigSource {
    Arc::new(|cwd: &Path| {
        let env = maho_config::process_env();
        let result = maho_config::load_native(cwd, &env);
        let side_panel_layers = result
            .layers
            .iter()
            .flat_map(|layer| {
                let mut found = Vec::new();
                if let Some(value) = layer.value.get("side_panel") {
                    found.push(value.clone());
                }
                for harness in ["native", "senpi"] {
                    if let Some(value) = layer
                        .value
                        .get(harness)
                        .and_then(|block| block.get("side_panel"))
                    {
                        found.push(value.clone());
                    }
                }
                found
            })
            .collect();
        LoadedConfig {
            view: result.view,
            side_panel_layers,
        }
    })
}

/// Validates the raw layers, then reads the merged view.
pub fn resolve_settings(config: &LoadedConfig) -> Result<Settings, SettingsError> {
    for layer in &config.side_panel_layers {
        Settings::from_value(Some(layer))?;
    }
    Settings::from_value(config.view.get("side_panel"))
}

/// What a session start does with the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Stay dark silently (disabled, or no interactive UI).
    Off,
    /// Stay dark and show this warning; the session still runs.
    Invalid(SettingsError),
    /// Mount with these settings.
    On(Settings),
}

/// The session-start decision (spec 1, 2): print/JSON/RPC modes and contexts without a UI stay
/// dark without reading config; invalid settings turn the panel off with a warning; otherwise the
/// flag wins over `side_panel.enabled`.
pub fn decide(
    mode: ExtensionMode,
    has_ui: bool,
    config: &LoadedConfig,
    flag: Option<bool>,
) -> Decision {
    if mode != ExtensionMode::Tui || !has_ui {
        return Decision::Off;
    }
    match resolve_settings(config) {
        Err(error) => Decision::Invalid(error),
        Ok(settings) if panel_enabled(settings.enabled, flag) => Decision::On(settings),
        Ok(_) => Decision::Off,
    }
}

/// The warning shown for invalid settings.
pub fn invalid_settings_warning(error: &SettingsError) -> String {
    format!("Side panel is off for this session: invalid setting {error}")
}

/// The `--omo-side-panel` / `--side-panel` override, if given.
pub fn flag_override(api: &ExtensionApi) -> Option<bool> {
    [FLAG_NAME, FLAG_ALIAS]
        .into_iter()
        .find_map(|name| match api.get_flag(name) {
            Some(FlagValue::Boolean(value)) => Some(value),
            Some(FlagValue::String(value)) => match value.as_str() {
                "true" | "1" | "on" | "yes" => Some(true),
                "false" | "0" | "off" | "no" => Some(false),
                _ => None,
            },
            None => None,
        })
}

/// Whether the host is in the fullscreen TUI layout (`tuiMode` in the agent settings).
fn fullscreen_layout(agent_dir: &Path) -> bool {
    std::fs::read_to_string(agent_dir.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("tuiMode")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|mode| mode == "fullscreen")
}

fn session_facts(ctx: &ExtensionContext) -> SessionFacts {
    let mut facts = SessionFacts {
        model_id: ctx
            .model
            .as_ref()
            .map(|m| format!("{}/{}", m.provider, m.id)),
        ..SessionFacts::default()
    };
    if let Ok(Some(usage)) = ctx.get_context_usage() {
        facts.context_window = Some(usage.context_window);
        facts.context_tokens = usage.tokens;
    }
    for entry in ctx.session_manager.get_branch() {
        if entry.kind != "message" {
            continue;
        }
        let message = &entry.data["message"];
        if message["role"] != "assistant" {
            continue;
        }
        let usage = &message["usage"];
        let input = usage["input"].as_u64().unwrap_or(0);
        let output = usage["output"].as_u64().unwrap_or(0);
        let cache_read = usage["cacheRead"].as_u64().unwrap_or(0);
        facts.input_tokens += input + cache_read;
        facts.output_tokens += output;
        facts.session_cost += usage["cost"]["total"].as_f64().unwrap_or(0.0);
        if input + cache_read > 0 {
            facts.cache_hit_rate = Some(cache_read as f64 / (input + cache_read) as f64);
        }
    }
    facts
}

/// The live session behind the panel.
struct SessionHost {
    ui: Arc<dyn ExtensionUi>,
    api: Arc<ExtensionApi>,
    column: ExtensionSideColumn,
    started: std::time::Instant,
    debug: bool,
    logger: Option<Arc<dyn maho_ext_api::ComponentLogger>>,
}

impl PanelHost for SessionHost {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn git(
        &self,
        args: Vec<String>,
        cwd: PathBuf,
        timeout_ms: u64,
    ) -> HostFuture<Result<ExecResult, ExecError>> {
        let api = Arc::clone(&self.api);
        Box::pin(async move {
            if api.runtime.session_actions().is_err() {
                return Err(ExecError::Unavailable);
            }
            api.exec(
                "git",
                &args,
                ExecOptions {
                    timeout_ms: Some(timeout_ms),
                    cwd: Some(cwd),
                    ..ExecOptions::default()
                },
            )
            .await
            .map_err(|error: ExtensionFailure| ExecError::Failed(error.message))
        })
    }

    fn repaint(&self) {
        let _not_available = self.ui.request_render();
    }

    fn mount(&self, _mode: MountMode) {
        // The host shows the column in fullscreen layout and falls back to a block above the
        // editor otherwise, so both modes hand over the same column.
        if let Err(error) = self.ui.set_side_column(Some(self.column.clone())) {
            self.debug(&format!("side panel: {}", error.message));
        }
    }

    fn unmount(&self) {
        let _not_available = self.ui.set_side_column(None);
    }

    fn notify(&self, message: &str, kind: NotificationType) {
        self.ui.notify(message, kind);
    }

    fn open_viewer(&self, viewer: Viewer) {
        let ui = Arc::clone(&self.ui);
        let viewer = Arc::new(Mutex::new(viewer));
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let factory: maho_ext_api::CustomComponentFactory =
                Arc::new(move |host, theme, _keys, done| {
                    let mut state = viewer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    state.terminal_rows = usize::from(host.dimensions().1);
                    let component = ViewerComponent {
                        viewer: state,
                        theme: theme.clone(),
                        done,
                    };
                    Box::pin(async move { Ok(Box::new(component) as Box<dyn Component>) })
                });
            let options = CustomUiFactoryOptions {
                overlay: true,
                ..CustomUiFactoryOptions::default()
            };
            let _closed = ui.custom_factory(factory, options).await;
        });
    }

    fn pick(&self, title: String, options: Vec<String>) -> HostFuture<Option<String>> {
        let ui = Arc::clone(&self.ui);
        Box::pin(async move {
            ui.select(&title, &options, ExtensionUiDialogOptions::default())
                .await
        })
    }

    fn debug(&self, message: &str) {
        if self.debug
            && let Some(logger) = &self.logger
        {
            logger.info(message, None);
        }
    }
}

/// The column component: renders the panel and turns clicks on linked rows into actions.
struct PanelComponent {
    panel: Arc<Panel>,
    theme: Theme,
    rows: Vec<String>,
}

impl Component for PanelComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let painter = theme_painter(&self.theme);
        self.rows = self.panel.render(width, &painter);
        self.rows.clone()
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.event_type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
            return None;
        }
        let row = self.rows.get(usize::try_from(event.y).ok()?)?;
        let start = row.find("\x1b]8;;")? + 5;
        let end = start + row[start..].find("\x1b\\")?;
        if !self.panel.handle_link(&row[start..end]) {
            return None;
        }
        Some(TuiMouseEventResult {
            handled: true,
            ..TuiMouseEventResult::default()
        })
    }
}

/// The viewer overlay component.
struct ViewerComponent {
    viewer: Viewer,
    theme: Theme,
    done: maho_ext_api::CustomUiDone,
}

impl Component for ViewerComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let theme = &self.theme;
        self.viewer
            .render(width, &|text: &str| theme.fg("error", text))
    }

    fn handle_input(&mut self, data: &str) {
        if self.viewer.handle_input(data) == KeyOutcome::Close {
            (self.done)(Value::Null);
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}

struct Shared {
    config: SidePanelExtension,
    api: Arc<ExtensionApi>,
    /// Command registration surface (registered on the first enabled session).
    registrar: Mutex<Option<ExtensionApi>>,
    panel: Mutex<Option<Arc<Panel>>>,
}

impl Shared {
    fn current(&self) -> Option<Arc<Panel>> {
        self.panel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn register_diff_command(self: &Arc<Self>) {
        let Some(mut api) = self
            .registrar
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let shared = Arc::clone(self);
        api.register_command(
            DIFF_COMMAND,
            Some("Pick a changed file and show its diff".into()),
            None,
            Arc::new(move |_args, ctx| {
                let shared = Arc::clone(&shared);
                Box::pin(async move {
                    match shared.current() {
                        Some(panel) => panel.diff_command().await,
                        None => ctx.ui.notify(
                            "The side panel is not shown in this session.",
                            NotificationType::Info,
                        ),
                    }
                    Ok(())
                })
            }),
        );
    }

    async fn session_start(self: &Arc<Self>, ctx: &ExtensionContext) {
        if let Some(previous) = self
            .panel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            previous.shutdown();
        }
        if ctx.mode != ExtensionMode::Tui || !ctx.has_ui {
            return;
        }
        let loaded = (self.config.config)(&ctx.cwd);
        let settings = match decide(ctx.mode, ctx.has_ui, &loaded, flag_override(&self.api)) {
            Decision::On(settings) => settings,
            Decision::Off => return,
            Decision::Invalid(error) => {
                ctx.ui
                    .notify(&invalid_settings_warning(&error), NotificationType::Warning);
                return;
            }
        };
        self.register_diff_command();
        let panel = self.build_panel(ctx, settings, &loaded.view);
        *self.panel.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&panel));
        panel.start();
        panel.update_facts(session_facts(ctx));
        panel.start_usage_poller();
        let background = Arc::clone(&panel);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { background.initial_load().await });
        }
    }

    fn build_panel(&self, ctx: &ExtensionContext, settings: Settings, view: &Value) -> Arc<Panel> {
        let env = &self.config.env;
        let session_id =
            Some(ctx.session_manager.session_id().to_owned()).filter(|s| !s.is_empty());
        let task_dir = session_id.as_deref().map(|id| {
            tasks::state_dir(
                &ctx.agent_dir,
                &ctx.cwd,
                view.pointer("/task/state_dir").and_then(Value::as_str),
                id,
            )
        });
        let memory_enabled = view
            .pointer("/memory/enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let memory: Option<MemoryReader> = self
            .config
            .memory
            .clone()
            .filter(|_| memory_enabled)
            .map(|status| {
                let cwd = ctx.cwd.clone();
                Arc::new(move || status(&cwd)) as MemoryReader
            });
        let panel_env = PanelEnv {
            cwd: ctx.cwd.clone(),
            home: env("HOME")
                .or_else(|| env("USERPROFILE"))
                .map(PathBuf::from),
            agent_dir: ctx.agent_dir.clone(),
            session_id,
            task_dir,
            goal_file: ctx.goal_store_file.clone(),
            fullscreen: fullscreen_layout(&ctx.agent_dir),
            links_supported: links::terminal_supports_links(&**env),
            memory,
            usage: self.config.usage.clone(),
        };
        // The column factory needs the panel, and the panel's host needs the column: tie the knot
        // through a weak slot filled right after construction (no reference cycle).
        let slot: Arc<Mutex<std::sync::Weak<Panel>>> = Arc::new(Mutex::new(std::sync::Weak::new()));
        let factory_slot = Arc::clone(&slot);
        let width_setting = settings.width;
        let column = ExtensionSideColumn {
            factory: Arc::new(move |theme: &Theme| {
                let panel = factory_slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .upgrade();
                match panel {
                    Some(panel) => Box::new(PanelComponent {
                        panel,
                        theme: theme.clone(),
                        rows: Vec::new(),
                    }) as Box<dyn Component>,
                    None => Box::new(maho_tui::tui::Container::new()) as Box<dyn Component>,
                }
            }),
            width: Arc::new(move |terminal: usize| {
                layout::effective_width(width_setting, terminal)
            }),
            min_columns: settings.min_columns as usize,
            min_transcript_columns: layout::MIN_TRANSCRIPT,
        };
        let host = SessionHost {
            ui: Arc::clone(&ctx.ui),
            api: Arc::clone(&self.api),
            column,
            started: std::time::Instant::now(),
            debug: env("MAHO_DEBUG").is_some(),
            logger: ctx.logger.clone(),
        };
        let panel = Panel::new(Arc::new(host), settings, panel_env);
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(&panel);
        panel
    }
}

impl Extension for SidePanelExtension {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            FLAG_NAME,
            FlagType::Boolean { default: None },
            Some("Show the side status panel (overrides side_panel.enabled)".into()),
        );
        api.register_flag(
            FLAG_ALIAS,
            FlagType::Boolean { default: None },
            Some("Alias of --omo-side-panel".into()),
        );
        let clone_api = |api: &ExtensionApi| {
            ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            )
        };
        let shared = Arc::new(Shared {
            config: self.clone(),
            api: Arc::new(clone_api(api)),
            registrar: Mutex::new(Some(clone_api(api))),
            panel: Mutex::new(None),
        });

        let on_start = Arc::clone(&shared);
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_event, ctx| {
                let shared = Arc::clone(&on_start);
                Box::pin(async move {
                    shared.session_start(ctx).await;
                    Ok(EventResult::None)
                })
            }),
        );

        for kind in [
            EventKind::TurnEnd,
            EventKind::AgentSettled,
            EventKind::MessageEnd,
        ] {
            let on_turn = Arc::clone(&shared);
            api.on(
                kind,
                Arc::new(move |_event, ctx| {
                    let shared = Arc::clone(&on_turn);
                    Box::pin(async move {
                        if let Some(panel) = shared.current() {
                            panel.after_turn(session_facts(ctx)).await;
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        let on_tool = Arc::clone(&shared);
        api.on(
            EventKind::ToolExecutionStart,
            Arc::new(move |event, _ctx| {
                let shared = Arc::clone(&on_tool);
                let call = match event {
                    ExtensionEvent::ToolExecutionStart {
                        tool_name, args, ..
                    } => Some((tool_name.clone(), args.clone())),
                    _ => None,
                };
                Box::pin(async move {
                    if let (Some(panel), Some((name, args))) = (shared.current(), call) {
                        panel.tool_started(&name, &args).await;
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let on_tool_end = Arc::clone(&shared);
        api.on(
            EventKind::ToolExecutionEnd,
            Arc::new(move |_event, _ctx| {
                let shared = Arc::clone(&on_tool_end);
                Box::pin(async move {
                    if let Some(panel) = shared.current() {
                        panel.tool_ended().await;
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let on_input = Arc::clone(&shared);
        api.on(
            EventKind::Input,
            Arc::new(move |_event, _ctx| {
                let shared = Arc::clone(&on_input);
                Box::pin(async move {
                    if let Some(panel) = shared.current() {
                        panel.user_input();
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let on_shutdown = Arc::clone(&shared);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_event, _ctx| {
                let shared = Arc::clone(&on_shutdown);
                Box::pin(async move {
                    let panel = shared
                        .panel
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    if let Some(panel) = panel {
                        panel.shutdown();
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}

/// Parses a link URL the panel would claim (re-exported for hosts with a link handler).
pub fn claim(url: &str) -> Option<Link> {
    Link::parse(url)
}
