use maho_ext_api::*;
use std::sync::{Arc, Mutex};

type PromptObserver = Arc<dyn Fn(ExtensionEvent) + Send + Sync>;
type PromptState = Arc<Mutex<(usize, Option<(UiPromptKind, Option<String>)>)>>;

pub struct LifecycleUi {
    pub inner: Arc<dyn ExtensionUi>,
    pub runtime: ExtensionRuntime,
    observer: PromptObserver,
    prompts: PromptState,
}
struct PromptGuard { state: PromptState, observer: PromptObserver }
impl Drop for PromptGuard {
    fn drop(&mut self) {
        let prompt = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.0 = state.0.saturating_sub(1);
            if state.0 == 0 { state.1.take() } else { None }
        };
        if let Some((kind, title)) = prompt { (self.observer)(ExtensionEvent::UiPromptEnd { kind, title }); }
    }
}
impl LifecycleUi {
    pub fn new(inner: Arc<dyn ExtensionUi>, runtime: ExtensionRuntime, observer: PromptObserver) -> Self {
        Self { inner, runtime, observer, prompts: Arc::default() }
    }
    fn begin(&self, kind: UiPromptKind, title: Option<&str>) -> Result<PromptGuard, ExtensionFailure> {
        self.runtime.assert_active()?;
        let title = title.filter(|title| !title.is_empty()).map(str::to_owned);
        let outer = {
            let mut state = self.prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let outer = state.0 == 0;
            state.0 += 1;
            if outer { state.1 = Some((kind, title.clone())); }
            outer
        };
        if outer { (self.observer)(ExtensionEvent::UiPromptStart { kind, title }); }
        Ok(PromptGuard { state: self.prompts.clone(), observer: self.observer.clone() })
    }
    fn active(&self) {
        if let Err(error) = self.runtime.assert_active() { std::panic::panic_any(error); }
    }
}
impl ExtensionUi for LifecycleUi {
    fn actions(&self) -> Option<&dyn ExtensionUiActions> { self.active(); self.inner.actions().map(|_| self as &dyn ExtensionUiActions) }
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { self.active(); self.inner.factories().map(|_| self as &dyn ExtensionUiFactories) }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String], dialog: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Select, Some(title)).unwrap_or_else(|error| std::panic::panic_any(error)); let result = self.inner.select(title, options, dialog).await; self.active(); result })
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str, dialog: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Confirm, Some(title)).unwrap_or_else(|error| std::panic::panic_any(error)); let result = self.inner.confirm(title, message, dialog).await; self.active(); result })
    }
    fn input<'a>(&'a self, title: &'a str, placeholder: Option<&'a str>, dialog: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Input, Some(title)).unwrap_or_else(|error| std::panic::panic_any(error)); let result = self.inner.input(title, placeholder, dialog).await; self.active(); result })
    }
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Editor, Some(title))?; let result = self.inner.editor(title, prefill).await?; self.runtime.assert_active()?; Ok(result) })
    }
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Question, None)?; let result = self.inner.question(request, options).await?; self.runtime.assert_active()?; Ok(result) })
    }
    fn custom(&self, factory: ComponentFactory, options: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Custom, None)?; let result = self.inner.custom(factory, options).await?; self.runtime.assert_active()?; Ok(result) })
    }
    fn custom_factory(&self, factory: CustomComponentFactory, options: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Custom, None)?; let result = self.inner.custom_factory(factory, options).await?; self.runtime.assert_active()?; Ok(result) })
    }
    fn notify(&self, message: &str, kind: NotificationType) { self.active(); self.inner.notify(message, kind); }
    fn set_status(&self, key: &str, text: Option<&str>) { self.active(); self.inner.set_status(key, text); }
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, options: ExtensionWidgetOptions) { self.active(); self.inner.set_widget(key, content, options); }
    fn set_header(&self, factory: Option<ComponentFactory>) { self.active(); self.inner.set_header(factory); }
    fn set_footer(&self, factory: Option<ComponentFactory>) { self.active(); self.inner.set_footer(factory); }
    fn set_title(&self, title: &str) { self.active(); self.inner.set_title(title); }
    fn paste_to_editor(&self, text: &str) { self.active(); self.inner.paste_to_editor(text); }
    fn set_editor_text(&self, text: &str) { self.active(); self.inner.set_editor_text(text); }
    fn get_editor_text(&self) -> String { self.active(); self.inner.get_editor_text() }
    fn theme(&self) -> Theme { self.active(); self.inner.theme() }
}

impl ExtensionUiFactories for LifecycleUi {
    fn set_widget_factory(&self, key: &str, factory: Option<TuiComponentFactory>, options: ExtensionWidgetOptions) {
        self.active();
        if let Some(factories) = self.inner.factories() { factories.set_widget_factory(key, factory, options); }
    }
    fn set_header_factory(&self, factory: Option<TuiComponentFactory>) {
        self.active();
        if let Some(factories) = self.inner.factories() { factories.set_header_factory(factory); }
    }
    fn set_footer_factory(&self, factory: Option<FooterComponentFactory>) {
        self.active();
        if let Some(factories) = self.inner.factories() { factories.set_footer_factory(factory); }
    }
    fn custom_factory(&self, factory: CustomComponentFactory, options: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> {
        ExtensionUi::custom_factory(self, factory, options)
    }
}

impl ExtensionUiActions for LifecycleUi {
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse> { ExtensionUi::question(self, request, options) }
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>> { ExtensionUi::editor(self, title, prefill) }
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> UiUnsubscribe { self.active(); self.inner.actions().expect("bound UI actions").on_terminal_input(handler) }
    fn set_working_message(&self, message: Option<&str>) { self.active(); self.inner.actions().expect("bound UI actions").set_working_message(message); }
    fn set_working_visible(&self, visible: bool) { self.active(); self.inner.actions().expect("bound UI actions").set_working_visible(visible); }
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) { self.active(); self.inner.actions().expect("bound UI actions").set_working_indicator(options); }
    fn set_hidden_thinking_label(&self, label: Option<&str>) { self.active(); self.inner.actions().expect("bound UI actions").set_hidden_thinking_label(label); }
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) { self.active(); self.inner.actions().expect("bound UI actions").add_autocomplete_provider(factory); }
    fn set_editor_component(&self, factory: Option<EditorFactory>) { self.active(); self.inner.actions().expect("bound UI actions").set_editor_component(factory); }
    fn get_editor_component(&self) -> Option<EditorFactory> { self.active(); self.inner.actions().expect("bound UI actions").get_editor_component() }
    fn get_all_themes(&self) -> Vec<ThemeInfo> { self.active(); self.inner.actions().expect("bound UI actions").get_all_themes() }
    fn get_theme(&self, name: &str) -> Option<Theme> { self.active(); self.inner.actions().expect("bound UI actions").get_theme(name) }
    fn set_theme(&self, theme: ThemeSelection) -> SetThemeResult { self.active(); self.inner.actions().expect("bound UI actions").set_theme(theme) }
    fn get_tools_expanded(&self) -> bool { self.active(); self.inner.actions().expect("bound UI actions").get_tools_expanded() }
    fn set_tools_expanded(&self, expanded: bool) { self.active(); self.inner.actions().expect("bound UI actions").set_tools_expanded(expanded); }
}
