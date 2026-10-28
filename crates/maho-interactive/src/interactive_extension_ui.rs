use maho_ext_api::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// The extension-facing theme. `maho_ext_api::Theme::colors`/`backgrounds` hold ANSI SGR prefixes
/// that consumers apply directly (see `crates/maho-ext-host/tests/notice.rs`), so every entry is the
/// theme's own `get_fg_ansi`/`get_bg_ansi` output rather than a hex value.
pub fn extension_theme(theme: &crate::theme::Theme) -> maho_ext_api::Theme {
    maho_ext_api::Theme {
        name: Some(theme.name.clone()),
        colors: crate::theme::ThemeColor::ALL
            .iter()
            .map(|color| (color.key().to_owned(), theme.get_fg_ansi(*color)))
            .collect(),
        backgrounds: crate::theme::ThemeBg::ALL
            .iter()
            .map(|background| (background.key().to_owned(), theme.get_bg_ansi(*background)))
            .collect(),
        vars: BTreeMap::new(),
    }
}

/// The inverse of [`extension_theme`]: rebuild a native theme from the ANSI SGR prefixes an
/// extension theme carries (`38;2;r;g;b` -> hex, `38;5;n` -> index, `39`/`49` -> terminal default).
pub fn theme_from_extension(
    theme: &maho_ext_api::Theme,
    mode: crate::theme::ColorMode,
) -> Result<crate::theme::Theme, crate::theme::theme_json::ThemeError> {
    use crate::theme::theme_json::ColorValue;
    let parse = |prefix: &str| -> Option<ColorValue> {
        let body = prefix.strip_prefix("\u{1b}[")?.strip_suffix('m')?;
        let parts: Vec<u8> = body
            .split(';')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        match parts.as_slice() {
            [38 | 48, 2, r, g, b] => Some(ColorValue::Text(format!("#{r:02x}{g:02x}{b:02x}"))),
            [38 | 48, 5, index] => Some(ColorValue::Index(*index)),
            [39] | [49] => Some(ColorValue::Text(String::new())),
            _ => None,
        }
    };
    let colors = theme
        .colors
        .iter()
        .chain(theme.backgrounds.iter())
        .filter_map(|(key, prefix)| parse(prefix).map(|value| (key.clone(), value)))
        .collect();
    let json = crate::theme::theme_json::ThemeJson {
        name: theme.name.clone().unwrap_or_else(|| "<in-memory>".into()),
        appearance: None,
        vars: BTreeMap::new(),
        colors,
        export_colors: BTreeMap::new(),
    };
    crate::theme::Theme::from_json(json, mode)
}

pub enum UiRequest {
    SubagentCost(Option<f64>),
    WidgetFrame,
    SideColumn(Option<ExtensionSideColumn>),
    Select {
        title: String,
        options: Vec<String>,
        timeout_ms: Option<u64>,
        reply: tokio::sync::oneshot::Sender<Option<String>>,
    },
    Input {
        title: String,
        timeout_ms: Option<u64>,
        reply: tokio::sync::oneshot::Sender<Option<String>>,
    },
    Notify(String, NotificationType),
    Widget(String, Option<WidgetContent>, ExtensionWidgetOptions),
    Header(Option<ComponentFactory>),
    Footer(Option<ComponentFactory>),
    WidgetFactory(String, Option<TuiComponentFactory>, ExtensionWidgetOptions),
    HeaderFactory(Option<TuiComponentFactory>),
    FooterFactory(Option<FooterComponentFactory>),
    CustomFactory(
        CustomComponentFactory,
        CustomUiFactoryOptions,
        tokio::sync::oneshot::Sender<Result<serde_json::Value, ExtensionFailure>>,
    ),
    Title(String),
    /// senpi `setThemeInstance`: apply an in-memory theme handed over by an extension.
    ThemeInstance(maho_ext_api::Theme),
    /// senpi `setTheme(name)`: `themeController.setThemeName`, then persist the global `theme` setting.
    ThemeName(String),
    Paste(String),
    EditorText(String),
    WorkingMessage(Option<String>),
    WorkingVisible(bool),
    WorkingIndicator(Option<WorkingIndicatorOptions>),
    Autocomplete(AutocompleteProviderFactory),
    EditorFactory(Option<EditorFactory>),
    HiddenThinkingLabel(Option<String>),
    ToolsExpanded(bool),
    SettingChanged(String, serde_json::Value),
    /// senpi `onThemePreview`: show a theme while `/settings` browses, without persisting it.
    ThemePreview(String),
    /// senpi `onTuiModeChange` / `onTerminalMouseChange`: a change that swaps the renderer, so it
    /// is persisted only once the host accepts it (no overlay is open).
    TerminalSetting(String, serde_json::Value),
    Editor {
        title: String,
        prefill: Option<String>,
        reply: tokio::sync::oneshot::Sender<Option<String>>,
    },
    Question {
        request: QuestionRequest,
        options: QuestionOptions,
        reply: tokio::sync::oneshot::Sender<QuestionResponse>,
    },
    /// senpi `resetExtensionUI`, queued by the before-session-invalidate hook so it lands ahead of
    /// the replacement session's own UI registrations.
    ResetExtensionUi,
}

pub struct InteractiveExtensionUi {
    pub sender: tokio::sync::mpsc::UnboundedSender<UiRequest>,
    pub editor_text: Mutex<String>,
    pub statuses: Mutex<BTreeMap<String, String>>,
    pub theme: Mutex<Theme>,
    pub terminal_input: Arc<Mutex<Vec<TerminalInputHandler>>>,
    pub tools_expanded: std::sync::atomic::AtomicBool,
    pub editor_factory: Mutex<Option<EditorFactory>>,
    pub theme_directory: Mutex<std::path::PathBuf>,
    /// senpi `getThemesDir()`: the shipped builtin theme directory, reported as builtin theme paths.
    pub builtin_theme_directory: Mutex<Option<std::path::PathBuf>>,
    /// Session event-bus publisher used to bracket host dialogs with `herdr:blocked`
    /// (senpi `withBlockedHostDialog`); unset when no session bus is bound.
    pub blocked_emitter: Mutex<Option<BlockedEmitter>>,
    /// Startup themes from packages and `--theme` (senpi `setRegisteredThemes`).
    pub registered_themes: Mutex<Vec<crate::theme::Theme>>,
    /// Themes contributed by extension `resources_discover` (senpi `extendResources` theme paths).
    pub discovered_themes: Mutex<Vec<crate::theme::Theme>>,
}

pub type BlockedEmitter = Arc<dyn Fn(&str, &serde_json::Value) + Send + Sync>;

static NEXT_BLOCKED_DIALOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Emits `herdr:blocked {active:false,id}` when the dialog settles or is dropped (the TS `finally`).
pub struct BlockedHostDialog {
    emit: BlockedEmitter,
    id: String,
}
impl Drop for BlockedHostDialog {
    fn drop(&mut self) {
        (self.emit)(
            "herdr:blocked",
            &serde_json::json!({"active": false, "id": self.id}),
        );
    }
}

impl InteractiveExtensionUi {
    fn send(&self, request: UiRequest) {
        drop(self.sender.send(request));
    }
    /// senpi `setBeforeSessionInvalidate(() => resetExtensionUI())`: the synchronously shared parts
    /// (`clearExtensionStatuses`, `clearExtensionTerminalInputListeners`) reset at once; the mode-owned
    /// parts reset when the queued request is drained.
    pub fn reset_before_session_invalidate(&self) {
        self.statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.terminal_input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.send(UiRequest::ResetExtensionUi);
    }
    /// senpi `withBlockedHostDialog`: announce the open host dialog before it is shown.
    fn block_host_dialog(&self, label: &str) -> Option<BlockedHostDialog> {
        let emit = self
            .blocked_emitter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let id = format!(
            "host-dialog-{}-{}",
            std::process::id(),
            NEXT_BLOCKED_DIALOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        emit(
            "herdr:blocked",
            &serde_json::json!({"active": true, "label": label, "id": id}),
        );
        Some(BlockedHostDialog { emit, id })
    }
    async fn wait<T: Default>(
        reply: tokio::sync::oneshot::Receiver<T>,
        options: ExtensionUiDialogOptions,
    ) -> T {
        let operation = async { reply.await.unwrap_or_default() };
        let bounded = async {
            if let Some(timeout) = options.timeout_ms {
                tokio::time::timeout(std::time::Duration::from_millis(timeout), operation)
                    .await
                    .unwrap_or_default()
            } else {
                operation.await
            }
        };
        if let Some(signal) = options.signal {
            tokio::select! { biased; () = signal.cancelled() => T::default(), value = bounded => value }
        } else {
            bounded.await
        }
    }
    /// A theme registry over the custom themes directory with the registered themes installed,
    /// then initialised to `name` (falling back to `dark` like [`ThemeRegistry::new`]).
    pub fn theme_registry(
        &self,
        directory: std::path::PathBuf,
        name: &str,
        mode: crate::theme::ColorMode,
    ) -> Result<crate::theme::registry::ThemeRegistry, crate::theme::theme_json::ThemeError> {
        let mut registry = crate::theme::registry::ThemeRegistry::new(directory, "dark", mode)?;
        registry.set_builtin_directory(
            self.builtin_theme_directory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        );
        let mut themes = self
            .registered_themes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for theme in self
            .discovered_themes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            if !themes.iter().any(|known| known.name == theme.name) {
                themes.push(theme.clone());
            }
        }
        registry.set_registered_themes(themes)?;
        registry.init_theme(name, false)?;
        Ok(registry)
    }
    /// Replace the extension-discovered themes with the ones loaded from `paths`; returns one
    /// diagnostic per path that fails to load.
    pub fn set_discovered_theme_paths(
        &self,
        paths: &[String],
        mode: crate::theme::ColorMode,
    ) -> Vec<String> {
        let mut themes: Vec<crate::theme::Theme> = Vec::new();
        let mut diagnostics = Vec::new();
        for path in paths {
            match crate::theme::Theme::load_from_path(std::path::Path::new(path), mode) {
                Ok(theme) => {
                    if !themes.iter().any(|known| known.name == theme.name) {
                        themes.push(theme);
                    }
                }
                Err(error) => diagnostics.push(format!("{path}: {error}")),
            }
        }
        *self
            .discovered_themes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = themes;
        diagnostics
    }
    pub fn channel(theme: Theme) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<UiRequest>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                editor_factory: Mutex::new(None),
                theme_directory: Mutex::new(Default::default()),
                builtin_theme_directory: Mutex::new(None),
                blocked_emitter: Mutex::new(None),
                registered_themes: Mutex::new(Vec::new()),
                discovered_themes: Mutex::new(Vec::new()),
                sender,
                editor_text: Mutex::new(String::new()),
                statuses: Mutex::new(BTreeMap::new()),
                theme: Mutex::new(theme),
                terminal_input: Arc::new(Mutex::new(Vec::new())),
                tools_expanded: std::sync::atomic::AtomicBool::new(false),
            }),
            receiver,
        )
    }
}

impl ExtensionUi for InteractiveExtensionUi {
    fn supports_subagent_cost(&self) -> bool {
        true
    }
    fn actions(&self) -> Option<&dyn ExtensionUiActions> {
        Some(self)
    }
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
        Some(self)
    }
    fn question(
        &self,
        request: QuestionRequest,
        options: QuestionOptions,
    ) -> ExtensionFuture<'_, QuestionResponse> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let mut dialog = options.dialog.clone();
        if options.get_deadline_at_ms.is_some() {
            dialog.timeout_ms = None;
        }
        let unanswered = request
            .questions
            .iter()
            .map(|question| question.id.clone())
            .collect();
        self.send(UiRequest::Question {
            request: request.clone(),
            options,
            reply,
        });
        Box::pin(async move {
            let mut status = QuestionStatus::Cancelled;
            let operation = async { receiver.await.ok() };
            let bounded = async {
                if let Some(timeout) = dialog.timeout_ms {
                    match tokio::time::timeout(std::time::Duration::from_millis(timeout), operation)
                        .await
                    {
                        Ok(response) => response,
                        Err(_) => {
                            status = QuestionStatus::TimedOut;
                            None
                        }
                    }
                } else {
                    operation.await
                }
            };
            let response = if let Some(signal) = dialog.signal {
                tokio::select! { biased; () = signal.cancelled() => None, response = bounded => response }
            } else {
                bounded.await
            };
            Ok(response.unwrap_or(QuestionResponse {
                status,
                unanswered,
                answers: Default::default(),
                comment: None,
                auto_resolved_after_ms: None,
                cancel_origin: None,
                resolved_by: None,
            }))
        })
    }
    fn get_tools_expanded(&self) -> Result<bool, ExtensionFailure> {
        Ok(self
            .tools_expanded
            .load(std::sync::atomic::Ordering::Relaxed))
    }
    fn set_tools_expanded(&self, expanded: bool) -> Result<(), ExtensionFailure> {
        self.tools_expanded
            .store(expanded, std::sync::atomic::Ordering::Relaxed);
        self.send(UiRequest::ToolsExpanded(expanded));
        Ok(())
    }
    fn on_terminal_input(
        &self,
        handler: TerminalInputHandler,
    ) -> Result<UiUnsubscribe, ExtensionFailure> {
        self.terminal_input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(handler.clone());
        let listeners = self.terminal_input.clone();
        Ok(Box::new(move || {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|listener| !Arc::ptr_eq(listener, &handler))
        }))
    }
    fn set_hidden_thinking_label(&self, label: Option<&str>) -> Result<(), ExtensionFailure> {
        self.send(UiRequest::HiddenThinkingLabel(label.map(str::to_owned)));
        Ok(())
    }
    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
    ) -> ExtensionFuture<'a, Option<String>> {
        let blocked = self.block_host_dialog(title);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Editor {
            title: title.into(),
            prefill: prefill.map(str::to_owned),
            reply,
        });
        Box::pin(async move {
            let _blocked = blocked;
            Ok(receiver.await.unwrap_or_default())
        })
    }
    fn set_working_message(&self, message: Option<&str>) -> Result<(), ExtensionFailure> {
        self.send(UiRequest::WorkingMessage(message.map(str::to_owned)));
        Ok(())
    }
    fn set_working_visible(&self, visible: bool) -> Result<(), ExtensionFailure> {
        self.send(UiRequest::WorkingVisible(visible));
        Ok(())
    }
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        let blocked = self.block_host_dialog(title);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Select {
            title: title.into(),
            options: options.to_vec(),
            timeout_ms: opts.timeout_ms,
            reply,
        });
        Box::pin(async move {
            let _blocked = blocked;
            Self::wait(receiver, opts).await
        })
    }
    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        let blocked = self.block_host_dialog(title);
        let title = format!("{title}\n{message}");
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Select {
            title,
            options: vec!["Yes".into(), "No".into()],
            timeout_ms: opts.timeout_ms,
            reply,
        });
        Box::pin(async move {
            let _blocked = blocked;
            Self::wait(receiver, opts).await.as_deref() == Some("Yes")
        })
    }
    fn input<'a>(
        &'a self,
        title: &'a str,
        _placeholder: Option<&'a str>,
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        let blocked = self.block_host_dialog(title);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Input {
            title: title.into(),
            timeout_ms: opts.timeout_ms,
            reply,
        });
        Box::pin(async move {
            let _blocked = blocked;
            Self::wait(receiver, opts).await
        })
    }
    fn request_render(&self) -> Result<(), ExtensionFailure> {
        self.sender
            .send(UiRequest::WidgetFrame)
            .map_err(|_| ExtensionFailure::new("Interactive UI request channel is closed"))
    }
    fn set_side_column(&self, column: Option<ExtensionSideColumn>) -> Result<(), ExtensionFailure> {
        self.sender
            .send(UiRequest::SideColumn(column))
            .map_err(|_| ExtensionFailure::new("Interactive UI request channel is closed"))
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.send(UiRequest::Notify(message.into(), kind));
    }
    fn set_subagent_cost(&self, cost_usd: Option<f64>) {
        self.send(UiRequest::SubagentCost(cost_usd));
    }
    fn set_status(&self, key: &str, text: Option<&str>) {
        let mut statuses = self
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(text) = text {
            statuses.insert(key.into(), text.into());
        } else {
            statuses.remove(key);
        }
        drop(statuses);
        // The footer reads the shared map; ask the host frame to show the change.
        self.send(UiRequest::WidgetFrame);
    }
    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: ExtensionWidgetOptions,
    ) {
        self.send(UiRequest::Widget(key.into(), content, options));
    }
    fn set_header(&self, factory: Option<ComponentFactory>) {
        self.send(UiRequest::Header(factory));
    }
    fn set_footer(&self, factory: Option<ComponentFactory>) {
        self.send(UiRequest::Footer(factory));
    }
    fn set_title(&self, title: &str) {
        self.send(UiRequest::Title(title.into()));
    }
    fn paste_to_editor(&self, text: &str) {
        self.send(UiRequest::Paste(text.into()));
    }
    fn set_editor_text(&self, text: &str) {
        *self
            .editor_text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = text.into();
        self.send(UiRequest::EditorText(text.into()));
    }
    fn get_editor_text(&self) -> String {
        self.editor_text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn custom(
        &self,
        _factory: ComponentFactory,
        _options: CustomUiOptions,
    ) -> ExtensionFuture<'_, serde_json::Value> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "ExtensionUi custom factory lacks completion callback and overlay handle contracts",
            ))
        })
    }
    fn theme(&self) -> Theme {
        self.theme
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ExtensionUiFactories for InteractiveExtensionUi {
    fn set_widget_factory(
        &self,
        key: &str,
        factory: Option<TuiComponentFactory>,
        options: ExtensionWidgetOptions,
    ) {
        self.send(UiRequest::WidgetFactory(key.into(), factory, options));
    }
    fn set_header_factory(&self, factory: Option<TuiComponentFactory>) {
        self.send(UiRequest::HeaderFactory(factory));
    }
    fn set_footer_factory(&self, factory: Option<FooterComponentFactory>) {
        self.send(UiRequest::FooterFactory(factory));
    }
    fn custom_factory(
        &self,
        factory: CustomComponentFactory,
        options: CustomUiFactoryOptions,
    ) -> ExtensionFuture<'_, serde_json::Value> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::CustomFactory(factory, options, reply));
        Box::pin(async move {
            receiver
                .await
                .map_err(|_| ExtensionFailure::new("Custom UI was disposed"))?
        })
    }
}

impl ExtensionUiActions for InteractiveExtensionUi {
    fn question(
        &self,
        request: QuestionRequest,
        options: QuestionOptions,
    ) -> ExtensionFuture<'_, QuestionResponse> {
        ExtensionUi::question(self, request, options)
    }
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> UiUnsubscribe {
        self.terminal_input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(handler.clone());
        let listeners = self.terminal_input.clone();
        Box::new(move || {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|listener| !Arc::ptr_eq(listener, &handler))
        })
    }
    fn set_working_message(&self, message: Option<&str>) {
        self.send(UiRequest::WorkingMessage(message.map(str::to_owned)));
    }
    fn set_working_visible(&self, visible: bool) {
        self.send(UiRequest::WorkingVisible(visible));
    }
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        self.send(UiRequest::WorkingIndicator(options));
    }
    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.send(UiRequest::HiddenThinkingLabel(label.map(str::to_owned)));
    }
    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
    ) -> ExtensionFuture<'a, Option<String>> {
        ExtensionUi::editor(self, title, prefill)
    }
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) {
        self.send(UiRequest::Autocomplete(factory));
    }
    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        *self
            .editor_factory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = factory.clone();
        self.send(UiRequest::EditorFactory(factory));
    }
    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.editor_factory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        let directory = self
            .theme_directory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.theme_registry(directory, "dark", crate::theme::detect_color_mode())
            .map(|registry| {
                registry
                    .get_available_themes_with_paths()
                    .into_iter()
                    .map(|theme| ThemeInfo {
                        name: theme.name,
                        path: theme.path,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    /// senpi `getThemeByName`: the named theme, or `None` when it cannot be loaded.
    fn get_theme(&self, name: &str) -> Option<Theme> {
        let directory = self
            .theme_directory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.theme_registry(directory, "dark", crate::theme::detect_color_mode())
            .and_then(|registry| registry.load_theme(name))
            .ok()
            .map(|theme| extension_theme(&theme))
    }
    fn set_theme(&self, theme: ThemeSelection) -> SetThemeResult {
        let name = match theme {
            ThemeSelection::Name(name) => name,
            ThemeSelection::Theme(theme) => {
                return match theme_from_extension(&theme, crate::theme::ColorMode::Truecolor) {
                    Ok(_) => {
                        self.send(UiRequest::ThemeInstance(theme));
                        SetThemeResult {
                            success: true,
                            error: None,
                        }
                    }
                    Err(error) => SetThemeResult {
                        success: false,
                        error: Some(error.to_string()),
                    },
                };
            }
        };
        let directory = self
            .theme_directory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Err(error) = self
            .theme_registry(directory, "dark", crate::theme::ColorMode::Truecolor)
            .and_then(|registry| registry.load_theme(&name))
        {
            return SetThemeResult {
                success: false,
                error: Some(error.to_string()),
            };
        }
        self.send(UiRequest::ThemeName(name));
        SetThemeResult {
            success: true,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        self.tools_expanded
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    fn set_tools_expanded(&self, expanded: bool) {
        self.tools_expanded
            .store(expanded, std::sync::atomic::Ordering::Relaxed);
        self.send(UiRequest::ToolsExpanded(expanded));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_theme_emits_the_ansi_prefixes_consumers_apply() {
        let theme = crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Truecolor)
            .expect("dark theme");
        let exported = extension_theme(&theme);
        assert_eq!(exported.name.as_deref(), Some("dark"));
        for key in ["accent", "dim", "warning", "success", "error", "text"] {
            let value = exported
                .colors
                .get(key)
                .unwrap_or_else(|| panic!("missing {key}"));
            assert!(
                value.starts_with('\u{1b}'),
                "{key} must be an ANSI prefix: {value:?}"
            );
            assert!(
                !value.contains('#'),
                "{key} must not carry a hex value: {value:?}"
            );
        }
        let background = exported
            .backgrounds
            .get("customMessageBg")
            .expect("customMessageBg");
        assert!(
            background.starts_with('\u{1b}'),
            "background must be an ANSI prefix: {background:?}"
        );
        assert_eq!(exported.colors.len(), crate::theme::ThemeColor::ALL.len());
        assert_eq!(exported.backgrounds.len(), crate::theme::ThemeBg::ALL.len());
    }

    #[tokio::test]
    async fn host_select_is_bracketed_by_herdr_blocked() {
        let (ui, mut requests) = InteractiveExtensionUi::channel(Theme::default());
        let events: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::default();
        let sink = events.clone();
        *ui.blocked_emitter.lock().expect("emitter slot") =
            Some(Arc::new(move |channel: &str, data: &serde_json::Value| {
                sink.lock()
                    .expect("events")
                    .push((channel.to_owned(), data.clone()))
            }));
        let options = vec!["a".to_owned()];
        let selected = ui.select("Pick", &options, Default::default());
        drop(requests.try_recv().expect("select request"));
        assert_eq!(selected.await, None);
        let events = events.lock().expect("events").clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "herdr:blocked");
        assert_eq!(events[0].1["active"], true);
        assert_eq!(events[0].1["label"], "Pick");
        assert_eq!(
            events[1].1,
            serde_json::json!({"active": false, "id": events[0].1["id"]})
        );
    }

    #[test]
    fn extension_discovered_themes_are_available_by_name() {
        let dir = tempfile::tempdir().expect("theme directory");
        let source = include_str!("theme/dark.json").replacen(
            "\"name\": \"dark\"",
            "\"name\": \"discovered\"",
            1,
        );
        let path = dir.path().join("discovered.json");
        std::fs::write(&path, source).expect("theme file");
        let (ui, _requests) = InteractiveExtensionUi::channel(maho_ext_api::Theme::default());
        *ui.theme_directory.lock().expect("directory") = dir.path().join("custom");
        assert!(
            ExtensionUiActions::get_all_themes(ui.as_ref())
                .iter()
                .all(|theme| theme.name != "discovered")
        );
        let diagnostics = ui.set_discovered_theme_paths(
            &[path.to_string_lossy().into_owned()],
            crate::theme::ColorMode::Truecolor,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(
            ExtensionUiActions::get_all_themes(ui.as_ref())
                .iter()
                .any(|theme| theme.name == "discovered")
        );
        assert_eq!(
            ExtensionUiActions::get_theme(ui.as_ref(), "discovered")
                .and_then(|theme| theme.name)
                .as_deref(),
            Some("discovered")
        );
    }

    #[test]
    fn unknown_theme_names_are_not_found() {
        let dir = tempfile::tempdir().expect("theme directory");
        let (ui, mut requests) = InteractiveExtensionUi::channel(maho_ext_api::Theme::default());
        *ui.theme_directory.lock().expect("directory") = dir.path().to_path_buf();
        assert!(ExtensionUiActions::get_theme(ui.as_ref(), "nope").is_none());
        let result =
            ExtensionUiActions::set_theme(ui.as_ref(), ThemeSelection::Name("nope".into()));
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("Theme not found: nope"));
        assert!(
            requests.try_recv().is_err(),
            "a failed switch must not reach the mode"
        );
    }

    #[test]
    fn registered_themes_are_listed_and_builtins_carry_their_path() {
        let (ui, _requests) = InteractiveExtensionUi::channel(maho_ext_api::Theme::default());
        let mut package = crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Truecolor)
            .expect("dark theme");
        package.name = "Package".into();
        *ui.registered_themes.lock().expect("registered") = vec![package];
        *ui.builtin_theme_directory
            .lock()
            .expect("builtin directory") = Some(std::path::PathBuf::from("/shipped/theme"));
        let themes = ExtensionUiActions::get_all_themes(ui.as_ref());
        let names: Vec<&str> = themes.iter().map(|theme| theme.name.as_str()).collect();
        assert_eq!(
            names,
            ["dark", "grok-day", "grok-night", "light", "Package"]
        );
        assert_eq!(
            themes[0].path.as_deref(),
            Some(std::path::Path::new("/shipped/theme/dark.json"))
        );
        assert!(ExtensionUiActions::get_theme(ui.as_ref(), "Package").is_some());
    }

    #[test]
    fn theme_instances_are_applied_rather_than_reduced_to_a_name() {
        let (ui, mut requests) = InteractiveExtensionUi::channel(maho_ext_api::Theme::default());
        let mut exported = extension_theme(
            &crate::theme::Theme::builtin("light", crate::theme::ColorMode::Truecolor)
                .expect("light theme"),
        );
        exported.name = Some("mine".into());
        assert!(
            ExtensionUiActions::set_theme(ui.as_ref(), ThemeSelection::Theme(exported.clone()))
                .success
        );
        let Ok(UiRequest::ThemeInstance(theme)) = requests.try_recv() else {
            panic!("expected a theme instance request")
        };
        let theme = theme_from_extension(&theme, crate::theme::ColorMode::Truecolor)
            .expect("instance theme");
        assert_eq!(theme.name, "mine");
        assert_eq!(extension_theme(&theme).colors, exported.colors);
    }

    #[test]
    fn extension_theme_matches_the_color_mode_of_its_theme() {
        let truecolor = extension_theme(
            &crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Truecolor)
                .expect("dark theme"),
        );
        let color256 = extension_theme(
            &crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Color256)
                .expect("dark theme"),
        );
        assert!(
            truecolor.colors["accent"].contains(";2;"),
            "truecolor uses RGB: {:?}",
            truecolor.colors["accent"]
        );
        assert!(
            color256.colors["accent"].contains(";5;"),
            "color256 uses an index: {:?}",
            color256.colors["accent"]
        );
    }
}
