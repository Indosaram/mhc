use async_trait::async_trait;
use maho_ai::{
    auth::types::{AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind},
    utils::abort::{AbortController, AbortReason},
};
use maho_ext_api::{ExtensionMode, ExtensionUi, ExtensionUiDialogOptions, NotificationType};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub const LOGIN_CANCELLED_MESSAGE: &str = "Login cancelled";

#[derive(Debug)]
pub struct LoginCancelled;

impl std::fmt::Display for LoginCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(LOGIN_CANCELLED_MESSAGE)
    }
}

impl std::error::Error for LoginCancelled {}

/// Keep retained ID tokens out of terminal output and notification transcripts (`oauth-url.ts`).
/// Browser launchers still receive the original URL.
pub fn redact_oauth_url(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return value.to_owned();
    };
    if !url.query_pairs().any(|(key, _)| key == "id_token_hint") {
        return value.to_owned();
    }
    let filtered: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "id_token_hint")
        .map(|(key, val)| (key.into_owned(), val.into_owned()))
        .collect();
    {
        let mut serializer = url.query_pairs_mut();
        serializer.clear();
        for (key, val) in &filtered {
            serializer.append_pair(key, val);
        }
    }
    if filtered.is_empty() {
        url.set_query(None);
    }
    url.to_string()
}

pub fn platform_open_browser() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|url: &str| {
        use std::process::{Command, Stdio};
        let mut command = if cfg!(target_os = "macos") {
            Command::new("open")
        } else if cfg!(target_os = "windows") {
            let mut command = Command::new("rundll32");
            command.arg("url.dll,FileProtocolHandler");
            command
        } else {
            Command::new("xdg-open")
        };
        command
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(mut child) = command.spawn() {
            std::thread::spawn(move || {
                let _result = child.wait();
            });
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
            let previous = pending
                .lock()
                .expect("pending login lock")
                .insert(provider_id.into(), Arc::downgrade(&controller));
            if let Some(previous) = previous.and_then(|previous| previous.upgrade()) {
                previous.abort(Some(AbortReason::new("Error", LOGIN_CANCELLED_MESSAGE)));
            }
            let provider_id = provider_id.to_owned();
            let current = Arc::downgrade(&controller);
            controller.signal().add_abort_listener(move |_| {
                let mut pending = pending.lock().expect("pending login lock");
                if pending
                    .get(&provider_id)
                    .is_some_and(|entry| entry.ptr_eq(&current))
                {
                    pending.remove(&provider_id);
                }
            });
        }
        Self {
            ui,
            mode,
            provider_label,
            controller,
            open_browser,
        }
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
        let login_listener =
            login_signal.add_abort_listener(move |reason| relay.abort(Some(reason.clone())));
        let prompt_listener = prompt.signal.as_ref().map(|source| {
            let relay = controller.clone();
            source.add_abort_listener(move |reason| relay.abort(Some(reason.clone())))
        });
        if login_signal.aborted()
            || prompt
                .signal
                .as_ref()
                .is_some_and(|source| source.aborted())
        {
            controller.abort(Some(AbortReason::new("Error", LOGIN_CANCELLED_MESSAGE)));
        }
        let result = async {
            if signal.aborted() {
                return Err(LoginCancelled.into());
            }
            let options = ExtensionUiDialogOptions {
                signal: Some(ui_signal),
                timeout_ms: None,
                ..Default::default()
            };
            let answer = match prompt.kind {
                AuthPromptKind::Select {
                    message,
                    options: choices,
                } => {
                    let labels: Vec<String> =
                        choices.iter().map(|choice| choice.label.clone()).collect();
                    let label = self.ui.select(&message, &labels, options).await;
                    choices
                        .into_iter()
                        .find(|choice| Some(&choice.label) == label.as_ref())
                        .map(|choice| choice.id)
                }
                AuthPromptKind::Text {
                    message,
                    placeholder,
                }
                | AuthPromptKind::ManualCode {
                    message,
                    placeholder,
                } => {
                    self.ui
                        .input(&message, placeholder.as_deref(), options)
                        .await
                }
                AuthPromptKind::Secret {
                    message,
                    placeholder,
                } => {
                    let mut secret_options = options;
                    secret_options.secret = self.ui.capabilities().secret_input;
                    self.ui
                        .input(&message, placeholder.as_deref(), secret_options)
                        .await
                }
            };
            if signal.aborted() {
                return Err(LoginCancelled.into());
            }
            match answer {
                Some(answer) => Ok(answer),
                None => {
                    self.controller
                        .abort(Some(AbortReason::new("Error", LOGIN_CANCELLED_MESSAGE)));
                    Err(LoginCancelled.into())
                }
            }
        }
        .await;
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
                if self.mode == ExtensionMode::Tui {
                    (self.open_browser)(&url);
                }
                let mut lines = vec![
                    format!("Open this URL to authorize {}:", self.provider_label),
                    redact_oauth_url(&url),
                ];
                if let Some(instructions) = instructions.filter(|value| !value.is_empty()) {
                    lines.push(instructions);
                }
                lines.join("\n")
            }
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                ..
            } => {
                format!(
                    "Open this URL to authorize {}:\n{verification_uri}\nEnter code: {user_code}",
                    self.provider_label
                )
            }
            AuthEvent::Info { message, links } => {
                let mut lines = vec![message];
                lines.extend(links.unwrap_or_default().into_iter().map(|link| {
                    match link.label.filter(|label| !label.is_empty()) {
                        Some(label) => format!("{label}: {}", link.url),
                        None => link.url,
                    }
                }));
                lines.join("\n")
            }
            AuthEvent::Progress { message } => message,
        };
        self.ui.notify(&message, NotificationType::Info);
    }
}
