//! Port of components/login-dialog.ts.
//!
//! senpi's promise-returning `showPrompt`/`showManualInput` become `tokio::sync::oneshot` receivers:
//! awaiting the receiver is awaiting the promise, and `cancel` drops the sender so the receiver
//! errors like the rejected promise. `openBrowser` and `tui.requestRender` arrive as host callbacks
//! (the utils and TUI modules are plan todos 35).

use maho_ai::auth::types::AuthInfoLink;
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, Focusable};
use tokio::sync::oneshot;

use super::keybinding_hints::key_hint;
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeColor};

/// senpi's `OAuthDeviceCodeInfo` (pi-ai), reduced to the fields the dialog renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDeviceCodeInfo {
    pub user_code: String,
    pub verification_uri: String,
}

enum Slot {
    Text(Text),
    /// senpi's `new Spacer(1)`: one blank line, no padding.
    Blank,
    Input,
}

/// senpi's `onComplete(success, message?)`.
pub type LoginCompleteHandler = Box<dyn FnMut(bool, Option<&str>)>;
pub type RenderRequest = Box<dyn FnMut()>;
pub type OpenBrowser = Box<dyn FnMut(&str)>;

pub struct LoginDialogComponent {
    theme: Theme,
    content: Vec<Slot>,
    input: Input,
    live_hint: Option<usize>,
    focused: bool,
    pending: Option<oneshot::Sender<String>>,
    cancelled: bool,
    on_complete: LoginCompleteHandler,
    request_render: Option<RenderRequest>,
    open_browser: Option<OpenBrowser>,
    /// senpi reads `process.platform === "darwin"` for the click hint.
    is_darwin: bool,
}

impl LoginDialogComponent {
    pub fn new(
        theme: &Theme,
        provider_id: &str,
        on_complete: LoginCompleteHandler,
        provider_name_override: Option<&str>,
        title_override: Option<&str>,
    ) -> Self {
        let provider_name = provider_name_override.unwrap_or(provider_id);
        let title = title_override
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Login to {provider_name}"));
        let title = theme.fg(ThemeColor::Accent, &theme.bold(&title));
        Self {
            theme: theme.clone(),
            content: vec![Slot::Text(Text::with_padding(title, 1, 0))],
            input: Input::new(InputOptions::default()),
            live_hint: None,
            focused: false,
            pending: None,
            cancelled: false,
            on_complete,
            request_render: None,
            open_browser: None,
            is_darwin: cfg!(target_os = "macos"),
        }
    }

    pub fn set_request_render(&mut self, request_render: RenderRequest) {
        self.request_render = Some(request_render);
    }

    pub fn set_open_browser(&mut self, open_browser: OpenBrowser) {
        self.open_browser = Some(open_browser);
    }

    pub fn input(&self) -> &Input {
        &self.input
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    fn request_render(&mut self) {
        if let Some(render) = &mut self.request_render {
            render();
        }
    }

    fn replace_input_with_submitted_text(&mut self, value: &str) {
        for slot in &mut self.content {
            if matches!(slot, Slot::Input) {
                *slot = Slot::Text(Text::with_padding(format!("> {value}"), 0, 0));
            }
        }
    }

    /// The Input widget is a single instance; mounting it twice paints two live `>` rows.
    fn remount_input(&mut self, hint: Text) {
        self.content.retain(|slot| !matches!(slot, Slot::Input));
        self.live_hint = None;
        self.content.push(Slot::Input);
        let index = self.content.len();
        self.content.push(Slot::Text(hint));
        self.live_hint = Some(index);
    }

    /// Exactly one `(to ...) ` hint row is ever live: a new one replaces the tracked previous hint.
    fn set_live_hint(&mut self, hint: Text) {
        if let Some(index) = self.live_hint.take()
            && index < self.content.len()
        {
            self.content.remove(index);
        }
        self.content.push(Slot::Text(hint));
        self.live_hint = Some(self.content.len() - 1);
    }

    /// Content that clears the dialog also drops any tracked live hint with it.
    fn reset_content(&mut self) {
        self.content.clear();
        self.live_hint = None;
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.pending = None;
        (self.on_complete)(false, Some("Login cancelled"));
    }

    /// Called by the onAuth callback - show URL and optional instructions.
    pub fn show_auth(&mut self, url: &str, instructions: Option<&str>) {
        self.reset_content();
        self.content.push(Slot::Blank);
        let linked_url = format!("\u{1b}]8;;{url}\u{7}{url}\u{1b}]8;;\u{7}");
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Accent, &linked_url),
            1,
            0,
        )));

        let click_hint = if self.is_darwin { "Cmd+click to open" } else { "Ctrl+click to open" };
        let hyperlink = format!("\u{1b}]8;;{url}\u{7}{click_hint}\u{1b}]8;;\u{7}");
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Dim, &hyperlink),
            1,
            0,
        )));

        if let Some(instructions) = instructions {
            self.content.push(Slot::Blank);
            self.content.push(Slot::Text(Text::with_padding(
                self.theme.fg(ThemeColor::Warning, instructions),
                1,
                0,
            )));
        }

        if let Some(open_browser) = &mut self.open_browser {
            open_browser(url);
        }
        self.request_render();
    }

    /// Called by the onDeviceCode callback - show URL and user code.
    pub fn show_device_code(&mut self, info: &OAuthDeviceCodeInfo) {
        self.reset_content();
        self.content.push(Slot::Blank);
        let linked_url = format!(
            "\u{1b}]8;;{}\u{7}{}\u{1b}]8;;\u{7}",
            info.verification_uri, info.verification_uri
        );
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Accent, &linked_url),
            1,
            0,
        )));

        let click_hint = if self.is_darwin { "Cmd+click to open" } else { "Ctrl+click to open" };
        let hyperlink = format!(
            "\u{1b}]8;;{}\u{7}{click_hint}\u{1b}]8;;\u{7}",
            info.verification_uri
        );
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Dim, &hyperlink),
            1,
            0,
        )));
        self.content.push(Slot::Blank);
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Warning, &format!("Enter code: {}", info.user_code)),
            1,
            0,
        )));

        self.request_render();
    }

    /// Show input for manual code/URL entry (for callback server providers).
    pub fn show_manual_input(&mut self, prompt: &str) -> oneshot::Receiver<String> {
        self.input.set_value("");
        self.content.push(Slot::Blank);
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Dim, prompt),
            1,
            0,
        )));
        let hint = Text::with_padding(
            format!("({})", key_hint(&self.theme, "tui.select.cancel", "to cancel")),
            1,
            0,
        );
        self.remount_input(hint);
        self.request_render();
        self.begin_pending()
    }

    /// Called by the onPrompt callback - show prompt and wait for input. Does NOT clear content.
    pub fn show_prompt(&mut self, message: &str, placeholder: Option<&str>) -> oneshot::Receiver<String> {
        self.content.push(Slot::Blank);
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Text, message),
            1,
            0,
        )));
        if let Some(placeholder) = placeholder {
            self.content.push(Slot::Text(Text::with_padding(
                self.theme.fg(ThemeColor::Dim, &format!("e.g., {placeholder}")),
                1,
                0,
            )));
        }
        let hint = Text::with_padding(
            format!(
                "({} {})",
                key_hint(&self.theme, "tui.select.cancel", "to cancel,"),
                key_hint(&self.theme, "tui.select.confirm", "to submit")
            ),
            1,
            0,
        );
        self.remount_input(hint);
        self.input.set_value("");
        self.request_render();
        self.begin_pending()
    }

    fn begin_pending(&mut self) -> oneshot::Receiver<String> {
        let (sender, receiver) = oneshot::channel();
        self.pending = Some(sender);
        receiver
    }

    /// Show informational text before another login step.
    pub fn show_details(&mut self, lines: &[String]) {
        self.reset_content();
        self.content.push(Slot::Blank);
        for line in lines {
            self.content.push(Slot::Text(Text::with_padding(line.clone(), 1, 0)));
        }
        self.request_render();
    }

    /// Show provider-owned information and links without starting an auth callback flow.
    pub fn show_info(&mut self, message: &str, links: &[AuthInfoLink], show_close_hint: bool) {
        self.content.push(Slot::Blank);
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Text, message),
            1,
            0,
        )));
        for link in links {
            let text = match &link.label {
                Some(label) => format!("{label}: {}", link.url),
                None => link.url.clone(),
            };
            let hyperlink = format!("\u{1b}]8;;{}\u{7}{text}\u{1b}]8;;\u{7}", link.url);
            self.content.push(Slot::Text(Text::with_padding(
                self.theme.fg(ThemeColor::Accent, &hyperlink),
                1,
                0,
            )));
        }
        if show_close_hint {
            self.content.push(Slot::Blank);
            let hint = Text::with_padding(
                format!("({})", key_hint(&self.theme, "tui.select.cancel", "to close")),
                1,
                0,
            );
            self.set_live_hint(hint);
        }
        self.request_render();
    }

    /// Show waiting message (for polling flows like GitHub Copilot).
    pub fn show_waiting(&mut self, message: &str) {
        self.content.push(Slot::Blank);
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Dim, message),
            1,
            0,
        )));
        let hint = Text::with_padding(
            format!("({})", key_hint(&self.theme, "tui.select.cancel", "to cancel")),
            1,
            0,
        );
        self.set_live_hint(hint);
        self.request_render();
    }

    /// Called by the onProgress callback.
    pub fn show_progress(&mut self, message: &str) {
        self.content.push(Slot::Text(Text::with_padding(
            self.theme.fg(ThemeColor::Dim, message),
            1,
            0,
        )));
        self.request_render();
    }

    fn submit(&mut self) {
        if let Some(sender) = self.pending.take() {
            let value = self.input.get_value().to_owned();
            self.replace_input_with_submitted_text(&value);
            let _ = sender.send(value);
        }
    }
}

impl Component for LoginDialogComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width)];
        for slot in &mut self.content {
            match slot {
                Slot::Text(text) => lines.extend(text.render(width)),
                Slot::Blank => lines.push(String::new()),
                Slot::Input => lines.extend(self.input.render(width)),
            }
        }
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.cancel") {
            self.cancel();
            return;
        }
        if kb.matches(data, "tui.select.confirm") {
            self.submit();
            return;
        }
        self.input.handle_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }

    fn invalidate(&mut self) {
        for slot in &mut self.content {
            if let Slot::Text(text) = slot {
                text.invalidate();
            }
        }
        self.input.invalidate();
    }
}

impl Focusable for LoginDialogComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.set_focused(value);
    }
}
