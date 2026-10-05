use async_trait::async_trait;
use maho_ai::{auth::types::{AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind}, utils::abort::{AbortController, AbortReason}};
use maho_ext_api::{ExtensionMode, ExtensionUi, ExtensionUiDialogOptions, NotificationType};
use std::{collections::HashMap, sync::{Arc, Mutex, OnceLock, Weak}};

pub const LOGIN_CANCELLED_MESSAGE: &str = "Login cancelled";

pub fn platform_open_browser() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|url: &str| {
        use std::process::{Command, Stdio};
        let mut command = if cfg!(target_os = "macos") { Command::new("open") } else if cfg!(target_os = "windows") { let mut command = Command::new("rundll32"); command.arg("url.dll,FileProtocolHandler"); command } else { Command::new("xdg-open") };
        command.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(mut child) = command.spawn() {
            std::thread::spawn(move || { let _result = child.wait(); });
        }
    })
}
type PendingLogins = Mutex<HashMap<String, Weak<AbortController>>>;
static PENDING_LOGINS: OnceLock<PendingLogins> = OnceLock::new();

pub struct ExtensionLoginInteraction {
    ui: Arc<dyn ExtensionUi>,
    mode: ExtensionMode,
    provider_label: String,
    controller: Arc<AbortController>,
    open_browser: Arc<dyn Fn(&str) + Send + Sync>,
}

impl ExtensionLoginInteraction {
    pub fn new(
        ui: Arc<dyn ExtensionUi>,
        mode: ExtensionMode,
        provider_label: String,
        provider_id: Option<&str>,
        open_browser: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        let controller = Arc::new(AbortController::new());
        if let Some(provider_id) = provider_id {
            let pending = PENDING_LOGINS.get_or_init(Mutex::default);
            let previous = pending.lock().expect("pending login lock")
                .insert(provider_id.into(), Arc::downgrade(&controller));
            if let Some(previous) = previous.and_then(|previous| previous.upgrade()) {
                previous.abort(Some(AbortReason::new("Error", LOGIN_CANCELLED_MESSAGE)));
            }
            let provider_id = provider_id.to_owned();
            let current = Arc::downgrade(&controller);
            controller.signal().add_abort_listener(move |_| {
                let mut pending = pending.lock().expect("pending login lock");
                if pending.get(&provider_id).is_some_and(|entry| entry.ptr_eq(&current)) {
                    pending.remove(&provider_id);
                }
            });
        }
        Self { ui, mode, provider_label, controller, open_browser }
    }
}

#[async_trait]
impl AuthInteraction for ExtensionLoginInteraction {
    fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> {
        Some(self.controller.signal())
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        let controller = AbortController::new();
        let signal = controller.signal();
        let ui_signal = maho_ext_api::AbortSignal::default();
        let ui_cancel = ui_signal.clone();
        let ui_listener = signal.add_abort_listener(move |_| ui_cancel.abort());
        let login_signal = self.controller.signal();
        let relay = controller.clone();
        let login_listener = login_signal.add_abort_listener(move |reason| relay.abort(Some(reason.clone())));
        let prompt_listener = prompt.signal.as_ref().map(|source| {
            let relay = controller.clone();
            source.add_abort_listener(move |reason| relay.abort(Some(reason.clone())))
        });
        if login_signal.aborted() || prompt.signal.as_ref().is_some_and(|source| source.aborted()) {
            controller.abort(Some(AbortReason::new("Error", LOGIN_CANCELLED_MESSAGE)));
        }
        let result = async {
            if signal.aborted() { anyhow::bail!(LOGIN_CANCELLED_MESSAGE); }
            let options = ExtensionUiDialogOptions { signal: Some(ui_signal), timeout_ms: None };
            let answer = match prompt.kind {
                AuthPromptKind::Select { message, options: choices } => {
                    let labels: Vec<String> = choices.iter().map(|choice| choice.label.clone()).collect();
                    let label = self.ui.select(&message, &labels, options).await;
                    choices.into_iter().find(|choice| Some(&choice.label) == label.as_ref()).map(|choice| choice.id)
                }
                AuthPromptKind::Text { message, placeholder }
                | AuthPromptKind::Secret { message, placeholder }
                | AuthPromptKind::ManualCode { message, placeholder } => {
                    self.ui.input(&message, placeholder.as_deref(), options).await
                }
            };
            if signal.aborted() { anyhow::bail!(LOGIN_CANCELLED_MESSAGE); }
            match answer {
                Some(answer) => Ok(answer),
                None => {
                    self.controller.abort(Some(AbortReason::new("Error", LOGIN_CANCELLED_MESSAGE)));
                    anyhow::bail!(LOGIN_CANCELLED_MESSAGE)
                }
            }
        }.await;
        signal.remove_abort_listener(ui_listener);
        login_signal.remove_abort_listener(login_listener);
        if let (Some(source), Some(listener)) = (prompt.signal, prompt_listener) {
            source.remove_abort_listener(listener);
        }
        result
    }

    fn notify(&self, event: AuthEvent) {
        let message = match event {
            AuthEvent::AuthUrl { url, instructions } => {
                if self.mode == ExtensionMode::Tui { (self.open_browser)(&url); }
                let mut lines = vec![format!("Open this URL to authorize {}:", self.provider_label), url];
                if let Some(instructions) = instructions.filter(|value| !value.is_empty()) { lines.push(instructions); }
                lines.join("\n")
            }
            AuthEvent::DeviceCode { user_code, verification_uri, .. } => {
                format!("Open this URL to authorize {}:\n{verification_uri}\nEnter code: {user_code}", self.provider_label)
            }
            AuthEvent::Info { message, links } => {
                let mut lines = vec![message];
                lines.extend(links.unwrap_or_default().into_iter().map(|link| match link.label.filter(|label| !label.is_empty()) {
                    Some(label) => format!("{label}: {}", link.url), None => link.url,
                }));
                lines.join("\n")
            }
            AuthEvent::Progress { message } => message,
        };
        self.ui.notify(&message, NotificationType::Info);
    }
}
