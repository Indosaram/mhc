use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use maho_ext_api::*;

pub enum UiRequest {
    Select { title: String, options: Vec<String>, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Input { title: String, reply: tokio::sync::oneshot::Sender<Option<String>> },
    Notify(String, NotificationType),
    Widget(String, Option<WidgetContent>, ExtensionWidgetOptions),
    Header(Option<ComponentFactory>),
    Footer(Option<ComponentFactory>),
    Title(String),
    Paste(String),
    EditorText(String),
    WorkingMessage(Option<String>),
    WorkingVisible(bool),
    HiddenThinkingLabel(Option<String>),
    ToolsExpanded(bool),
    Editor { title: String, prefill: Option<String>, reply: tokio::sync::oneshot::Sender<Option<String>> },
}

pub struct InteractiveExtensionUi {
    pub sender: tokio::sync::mpsc::UnboundedSender<UiRequest>,
    pub editor_text: Mutex<String>,
    pub statuses: Mutex<BTreeMap<String, String>>,
    pub theme: Mutex<Theme>,
    pub terminal_input: Arc<Mutex<Vec<TerminalInputHandler>>>,
    pub tools_expanded: std::sync::atomic::AtomicBool,
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
        (Arc::new(Self { sender, editor_text: Mutex::new(String::new()), statuses: Mutex::new(BTreeMap::new()), theme: Mutex::new(theme), terminal_input:Arc::new(Mutex::new(Vec::new())), tools_expanded:std::sync::atomic::AtomicBool::new(false) }), receiver)
    }
}

impl ExtensionUi for InteractiveExtensionUi {
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
