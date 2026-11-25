//! Forwarding UI context wrapper for in-process children (`runners/child-dialogs/forwarding-ui-context.ts`).

use std::sync::Arc;

use super::forwardable_dialog::{ForwardableDialog, ForwardedAnswer};
use super::forwarder::{ChildDialogForwarder, ChildDialogSignal, ForwardedChild};

#[derive(Debug, Clone, Default)]
pub struct PromptDialogOptions {
    pub signal: Option<ChildDialogSignal>,
    pub timeout: Option<u64>,
    pub tool_call_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub secret: bool,
}

/// Host dialog prompting surface mirrored by `ExtensionUIContext` dialog methods.
pub trait DialogPromptUi: Send + Sync {
    fn select(&self, title: &str, options: &[String], opts: &PromptDialogOptions)
    -> Option<String>;
    fn confirm(&self, title: &str, message: &str, opts: &PromptDialogOptions) -> bool;
    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: &PromptDialogOptions,
    ) -> Option<String>;
    fn editor(
        &self,
        title: &str,
        prefill: Option<&str>,
        opts: &PromptDialogOptions,
    ) -> Option<String>;
}

/// Wraps a headless UI context, forwarding the four prompting methods to the parent's UI while
/// leaving `secret: true` inputs on the underlying headless context.
pub struct ForwardingDialogUi {
    headless: Arc<dyn DialogPromptUi>,
    forwarder: Arc<dyn ChildDialogForwarder>,
    task_id: String,
}

impl ForwardingDialogUi {
    fn forward(&self, dialog: ForwardableDialog, opts: &PromptDialogOptions) -> ForwardedAnswer {
        self.forwarder.forward(
            dialog,
            ForwardedChild {
                task_id: self.task_id.clone(),
                signal: opts.signal.clone().unwrap_or_default(),
            },
        )
    }
}

impl DialogPromptUi for ForwardingDialogUi {
    fn select(
        &self,
        title: &str,
        options: &[String],
        opts: &PromptDialogOptions,
    ) -> Option<String> {
        match self.forward(
            ForwardableDialog::Select {
                title: title.to_string(),
                options: options.to_vec(),
                tool_call_id: opts.tool_call_id.clone(),
                parent_tool_call_id: opts.parent_tool_call_id.clone(),
                timeout: opts.timeout,
            },
            opts,
        ) {
            ForwardedAnswer::Value(value) => Some(value),
            _ => None,
        }
    }

    fn confirm(&self, title: &str, message: &str, opts: &PromptDialogOptions) -> bool {
        matches!(
            self.forward(
                ForwardableDialog::Confirm {
                    title: title.to_string(),
                    message: message.to_string(),
                    timeout: opts.timeout,
                },
                opts,
            ),
            ForwardedAnswer::Confirmed(true)
        )
    }

    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: &PromptDialogOptions,
    ) -> Option<String> {
        if opts.secret {
            return self.headless.input(title, placeholder, opts);
        }
        match self.forward(
            ForwardableDialog::Input {
                title: title.to_string(),
                placeholder: placeholder.map(str::to_string),
                tool_call_id: opts.tool_call_id.clone(),
                parent_tool_call_id: opts.parent_tool_call_id.clone(),
                timeout: opts.timeout,
            },
            opts,
        ) {
            ForwardedAnswer::Value(value) => Some(value),
            _ => None,
        }
    }

    fn editor(
        &self,
        title: &str,
        prefill: Option<&str>,
        opts: &PromptDialogOptions,
    ) -> Option<String> {
        match self.forward(
            ForwardableDialog::Editor {
                title: title.to_string(),
                prefill: prefill.map(str::to_string),
            },
            opts,
        ) {
            ForwardedAnswer::Value(value) => Some(value),
            _ => None,
        }
    }
}

pub fn create_forwarding_ui_context(
    headless: Arc<dyn DialogPromptUi>,
    forwarder: Arc<dyn ChildDialogForwarder>,
    task_id: impl Into<String>,
) -> Arc<dyn DialogPromptUi> {
    Arc::new(ForwardingDialogUi {
        headless,
        forwarder,
        task_id: task_id.into(),
    })
}
