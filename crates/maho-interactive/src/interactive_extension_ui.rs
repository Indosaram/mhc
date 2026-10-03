use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use maho_ext_api::*;

/// The extension-facing theme. `maho_ext_api::Theme::colors`/`backgrounds` hold ANSI SGR prefixes
/// that consumers apply directly (see `crates/maho-ext-host/tests/notice.rs`), so every entry is the
/// theme's own `get_fg_ansi`/`get_bg_ansi` output rather than a hex value.
pub fn extension_theme(theme: &crate::theme::Theme) -> maho_ext_api::Theme {
    maho_ext_api::Theme {
        name: Some(theme.name.clone()),
        colors: crate::theme::ThemeColor::ALL.iter().map(|color| (color.key().to_owned(), theme.get_fg_ansi(*color))).collect(),
        backgrounds: crate::theme::ThemeBg::ALL.iter().map(|background| (background.key().to_owned(), theme.get_bg_ansi(*background))).collect(),
        vars: BTreeMap::new(),
    }
}

pub enum UiRequest {
    WidgetFrame,
    Select { title: String, options: Vec<String>, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Input { title: String, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Notify(String, NotificationType),
    Widget(String, Option<WidgetContent>, ExtensionWidgetOptions),
    Header(Option<ComponentFactory>),
    Footer(Option<ComponentFactory>),
    WidgetFactory(String, Option<TuiComponentFactory>, ExtensionWidgetOptions),
    HeaderFactory(Option<TuiComponentFactory>),
    FooterFactory(Option<FooterComponentFactory>),
    CustomFactory(CustomComponentFactory, CustomUiFactoryOptions, tokio::sync::oneshot::Sender<Result<serde_json::Value, ExtensionFailure>>),
    Title(String),
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
    Editor { title: String, prefill: Option<String>, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Question { request: QuestionRequest, options: QuestionOptions, reply: tokio::sync::oneshot::Sender<QuestionResponse> },
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
}

impl InteractiveExtensionUi {
    fn send(&self, request: UiRequest) { drop(self.sender.send(request)); }
    async fn wait<T: Default>(reply: tokio::sync::oneshot::Receiver<T>, options: ExtensionUiDialogOptions) -> T {
        let operation = async { reply.await.unwrap_or_default() };
        let bounded = async {
            if let Some(timeout) = options.timeout_ms { tokio::time::timeout(std::time::Duration::from_millis(timeout), operation).await.unwrap_or_default() }
            else { operation.await }
        };
        if let Some(signal) = options.signal { tokio::select! { biased; () = signal.cancelled() => T::default(), value = bounded => value } }
        else { bounded.await }
    }
    pub fn channel(theme: Theme) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<UiRequest>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(Self { editor_factory:Mutex::new(None), theme_directory:Mutex::new(Default::default()), sender, editor_text: Mutex::new(String::new()), statuses: Mutex::new(BTreeMap::new()), theme: Mutex::new(theme), terminal_input:Arc::new(Mutex::new(Vec::new())), tools_expanded:std::sync::atomic::AtomicBool::new(false) }), receiver)
    }
}

impl ExtensionUi for InteractiveExtensionUi {
    fn actions(&self) -> Option<&dyn ExtensionUiActions> { Some(self) }
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { Some(self) }
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let mut dialog = options.dialog.clone();
        if options.get_deadline_at_ms.is_some() { dialog.timeout_ms = None; }
        let unanswered = request.questions.iter().map(|question| question.id.clone()).collect();
        self.send(UiRequest::Question { request:request.clone(), options, reply });
        Box::pin(async move {
            let mut status = QuestionStatus::Cancelled;
            let operation = async { receiver.await.ok() };
            let bounded = async { if let Some(timeout) = dialog.timeout_ms { match tokio::time::timeout(std::time::Duration::from_millis(timeout), operation).await { Ok(response) => response, Err(_) => { status = QuestionStatus::TimedOut; None } } } else { operation.await } };
            let response = if let Some(signal) = dialog.signal { tokio::select! { biased; () = signal.cancelled() => None, response = bounded => response } } else { bounded.await };
            Ok(response.unwrap_or(QuestionResponse { status, unanswered, answers:Default::default(), comment:None, auto_resolved_after_ms:None }))
        })
    }
    fn get_tools_expanded(&self) -> Result<bool, ExtensionFailure> { Ok(self.tools_expanded.load(std::sync::atomic::Ordering::Relaxed)) }
    fn set_tools_expanded(&self, expanded: bool) -> Result<(), ExtensionFailure> { self.tools_expanded.store(expanded, std::sync::atomic::Ordering::Relaxed); self.send(UiRequest::ToolsExpanded(expanded)); Ok(()) }
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Result<UiUnsubscribe, ExtensionFailure> {
        self.terminal_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(handler.clone());
        let listeners = self.terminal_input.clone();
        Ok(Box::new(move || listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|listener| !Arc::ptr_eq(listener, &handler))))
    }
    fn set_hidden_thinking_label(&self, label: Option<&str>) -> Result<(), ExtensionFailure> { self.send(UiRequest::HiddenThinkingLabel(label.map(str::to_owned))); Ok(()) }
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Editor { title:title.into(), prefill:prefill.map(str::to_owned), reply });
        Box::pin(async move { Ok(receiver.await.unwrap_or_default()) })
    }
    fn set_working_message(&self, message: Option<&str>) -> Result<(), ExtensionFailure> { self.send(UiRequest::WorkingMessage(message.map(str::to_owned))); Ok(()) }
    fn set_working_visible(&self, visible: bool) -> Result<(), ExtensionFailure> { self.send(UiRequest::WorkingVisible(visible)); Ok(()) }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String], opts: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Select { title: title.into(), options: options.to_vec(), reply });
        Box::pin(Self::wait(receiver, opts))
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str, opts: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        let title = format!("{title}\n{message}");
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Select { title, options: vec!["Yes".into(), "No".into()], reply });
        Box::pin(async move { Self::wait(receiver, opts).await.as_deref() == Some("Yes") })
    }
    fn input<'a>(&'a self, title: &'a str, _placeholder: Option<&'a str>, opts: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::Input { title: title.into(), reply });
        Box::pin(Self::wait(receiver, opts))
    }
    fn notify(&self, message: &str, kind: NotificationType) { self.send(UiRequest::Notify(message.into(), kind)); }
    fn set_status(&self, key: &str, text: Option<&str>) {
        let mut statuses = self.statuses.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(text) = text { statuses.insert(key.into(), text.into()); } else { statuses.remove(key); }
    }
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, options: ExtensionWidgetOptions) { self.send(UiRequest::Widget(key.into(), content, options)); }
    fn set_header(&self, factory: Option<ComponentFactory>) { self.send(UiRequest::Header(factory)); }
    fn set_footer(&self, factory: Option<ComponentFactory>) { self.send(UiRequest::Footer(factory)); }
    fn set_title(&self, title: &str) { self.send(UiRequest::Title(title.into())); }
    fn paste_to_editor(&self, text: &str) { self.send(UiRequest::Paste(text.into())); }
    fn set_editor_text(&self, text: &str) {
        *self.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = text.into();
        self.send(UiRequest::EditorText(text.into()));
    }
    fn get_editor_text(&self) -> String { self.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }
    fn custom(&self, _factory: ComponentFactory, _options: CustomUiOptions) -> ExtensionFuture<'_, serde_json::Value> {
        Box::pin(async { Err(ExtensionFailure::new("ExtensionUi custom factory lacks completion callback and overlay handle contracts")) })
    }
    fn theme(&self) -> Theme { self.theme.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }
}

impl ExtensionUiFactories for InteractiveExtensionUi {
    fn set_widget_factory(&self, key: &str, factory: Option<TuiComponentFactory>, options: ExtensionWidgetOptions) { self.send(UiRequest::WidgetFactory(key.into(), factory, options)); }
    fn set_header_factory(&self, factory: Option<TuiComponentFactory>) { self.send(UiRequest::HeaderFactory(factory)); }
    fn set_footer_factory(&self, factory: Option<FooterComponentFactory>) { self.send(UiRequest::FooterFactory(factory)); }
    fn custom_factory(&self, factory: CustomComponentFactory, options: CustomUiFactoryOptions) -> ExtensionFuture<'_, serde_json::Value> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.send(UiRequest::CustomFactory(factory, options, reply));
        Box::pin(async move { receiver.await.map_err(|_| ExtensionFailure::new("Custom UI was disposed"))? })
    }
}

impl ExtensionUiActions for InteractiveExtensionUi {
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse> { ExtensionUi::question(self, request, options) }
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> UiUnsubscribe {
        self.terminal_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(handler.clone());
        let listeners = self.terminal_input.clone();
        Box::new(move || listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|listener| !Arc::ptr_eq(listener, &handler)))
    }
    fn set_working_message(&self, message: Option<&str>) { self.send(UiRequest::WorkingMessage(message.map(str::to_owned))); }
    fn set_working_visible(&self, visible: bool) { self.send(UiRequest::WorkingVisible(visible)); }
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) { self.send(UiRequest::WorkingIndicator(options)); }
    fn set_hidden_thinking_label(&self, label: Option<&str>) { self.send(UiRequest::HiddenThinkingLabel(label.map(str::to_owned))); }
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>> { ExtensionUi::editor(self, title, prefill) }
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) { self.send(UiRequest::Autocomplete(factory)); }
    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        *self.editor_factory.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = factory.clone();
        self.send(UiRequest::EditorFactory(factory));
    }
    fn get_editor_component(&self) -> Option<EditorFactory> { self.editor_factory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }
    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        let directory = self.theme_directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        crate::theme::registry::ThemeRegistry::new(directory, "dark", crate::theme::ColorMode::Truecolor)
            .map(|registry| registry.get_available_themes_with_paths().into_iter().map(|theme| ThemeInfo { name:theme.name, path:theme.path }).collect()).unwrap_or_default()
    }
    fn get_theme(&self, name: &str) -> Option<Theme> {
        let directory = self.theme_directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        crate::theme::registry::ThemeRegistry::new(directory, name, crate::theme::ColorMode::Truecolor).ok()
            .map(|registry| extension_theme(&registry.current))
    }
    fn set_theme(&self, theme: ThemeSelection) -> SetThemeResult {
        let name = match theme { ThemeSelection::Name(name) => name, ThemeSelection::Theme(theme) => theme.name.unwrap_or_default() };
        if ExtensionUiActions::get_theme(self, &name).is_none() { return SetThemeResult { success:false, error:Some(format!("Theme not found: {name}")) }; }
        self.send(UiRequest::SettingChanged("theme".into(), serde_json::json!(name)));
        SetThemeResult { success:true, error:None }
    }
    fn get_tools_expanded(&self) -> bool { self.tools_expanded.load(std::sync::atomic::Ordering::Relaxed) }
    fn set_tools_expanded(&self, expanded: bool) { self.tools_expanded.store(expanded, std::sync::atomic::Ordering::Relaxed); self.send(UiRequest::ToolsExpanded(expanded)); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_theme_emits_the_ansi_prefixes_consumers_apply() {
        let theme = crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Truecolor).expect("dark theme");
        let exported = extension_theme(&theme);
        assert_eq!(exported.name.as_deref(), Some("dark"));
        for key in ["accent", "dim", "warning", "success", "error", "text"] {
            let value = exported.colors.get(key).unwrap_or_else(|| panic!("missing {key}"));
            assert!(value.starts_with('\u{1b}'), "{key} must be an ANSI prefix: {value:?}");
            assert!(!value.contains('#'), "{key} must not carry a hex value: {value:?}");
        }
        let background = exported.backgrounds.get("customMessageBg").expect("customMessageBg");
        assert!(background.starts_with('\u{1b}'), "background must be an ANSI prefix: {background:?}");
        assert_eq!(exported.colors.len(), crate::theme::ThemeColor::ALL.len());
        assert_eq!(exported.backgrounds.len(), crate::theme::ThemeBg::ALL.len());
    }

    #[test]
    fn extension_theme_matches_the_color_mode_of_its_theme() {
        let truecolor = extension_theme(&crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Truecolor).expect("dark theme"));
        let color256 = extension_theme(&crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Color256).expect("dark theme"));
        assert!(truecolor.colors["accent"].contains(";2;"), "truecolor uses RGB: {:?}", truecolor.colors["accent"]);
        assert!(color256.colors["accent"].contains(";5;"), "color256 uses an index: {:?}", color256.colors["accent"]);
    }
}
