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
    fn actions(&self) -> Option<&dyn ExtensionUiActions> { self.active(); self.inner.actions() }
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { self.active(); self.inner.factories() }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String], dialog: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Select, Some(title)).unwrap_or_else(|error| std::panic::panic_any(error)); self.inner.select(title, options, dialog).await })
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str, dialog: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Confirm, Some(title)).unwrap_or_else(|error| std::panic::panic_any(error)); self.inner.confirm(title, message, dialog).await })
    }
    fn input<'a>(&'a self, title: &'a str, placeholder: Option<&'a str>, dialog: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Input, Some(title)).unwrap_or_else(|error| std::panic::panic_any(error)); self.inner.input(title, placeholder, dialog).await })
    }
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Editor, Some(title))?; self.inner.editor(title, prefill).await })
    }
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Question, None)?; self.inner.question(request, options).await })
    }
    fn custom(&self, factory: ComponentFactory, options: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Custom, None)?; self.inner.custom(factory, options).await })
    }
    fn custom_factory(&self, factory: CustomComponentFactory, options: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async move { let _guard = self.begin(UiPromptKind::Custom, None)?; self.inner.custom_factory(factory, options).await })
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
