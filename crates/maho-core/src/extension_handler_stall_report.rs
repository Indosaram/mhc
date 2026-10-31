//! Port of senpi `packages/coding-agent/src/core/extension-handler-stall-report.ts`.
//!
//! `ExtensionHandlerStall` mirrors the maho-ext-host handler budget's stall (senpi
//! `extensions/handler-budget.ts`); AgentSession converts each published stall and reports it here.

use std::path::Path;

use serde_json::{Map, Value};

use crate::session_log::SessionLogger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionHandlerStallStage {
    Warn,
    Timeout,
    Released,
    LateFailure,
}

impl ExtensionHandlerStallStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Timeout => "timeout",
            Self::Released => "released",
            Self::LateFailure => "late_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionHandlerStall {
    pub extension_path: String,
    pub event: String,
    pub stage: ExtensionHandlerStallStage,
    pub elapsed_ms: u64,
    pub timeout_ms: u64,
    pub error: Option<String>,
}

impl From<&maho_ext_host::runner::handler_budget::ExtensionHandlerStall> for ExtensionHandlerStall {
    fn from(stall: &maho_ext_host::runner::handler_budget::ExtensionHandlerStall) -> Self {
        use maho_ext_host::runner::handler_budget::ExtensionHandlerStallStage as Stage;
        Self {
            extension_path: stall.extension_path.clone(),
            event: stall.event.clone(),
            stage: match stall.stage {
                Stage::Warn => ExtensionHandlerStallStage::Warn,
                Stage::Timeout => ExtensionHandlerStallStage::Timeout,
                Stage::Released => ExtensionHandlerStallStage::Released,
                Stage::LateFailure => ExtensionHandlerStallStage::LateFailure,
            },
            elapsed_ms: stall.elapsed_ms,
            timeout_ms: stall.timeout_ms,
            error: stall.error.clone(),
        }
    }
}

fn seconds(ms: u64) -> String {
    format!("{} s", ((ms as f64 / 1000.0).round() as u64).max(1))
}

/// The notice a stalled handler shows: only the `warn` stage is shown, because the turn is waiting on it.
pub fn extension_handler_stall_notice(stall: &ExtensionHandlerStall) -> Option<String> {
    if stall.stage != ExtensionHandlerStallStage::Warn {
        return None;
    }
    let extension = basename(&stall.extension_path);
    let cap = if stall.timeout_ms > 0 {
        format!(
            " The session continues without it after {}.",
            seconds(stall.timeout_ms)
        )
    } else {
        " Action: set extensionEventHandlerTimeoutMs to bound it, or remove the extension."
            .to_owned()
    };
    Some(format!(
        "Extension {extension} is still running its {} handler after {}, so this turn is waiting on it.{cap}",
        stall.event,
        seconds(stall.elapsed_ms)
    ))
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

/// The fields every stage logs to session.log.
pub fn extension_handler_stall_log_fields(stall: &ExtensionHandlerStall) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("extension".into(), basename(&stall.extension_path).into());
    fields.insert("handlerEvent".into(), stall.event.clone().into());
    fields.insert("stage".into(), stall.stage.as_str().into());
    fields.insert("durationMs".into(), stall.elapsed_ms.into());
    if let Some(error) = &stall.error {
        fields.insert("error".into(), error.clone().into());
    }
    fields
}

/// Makes a stalled extension handler diagnosable (senpi#3650): every stage lands in session.log with the
/// event and extension, and a handler still running past the warning is shown. A timeout or deadline
/// release is shown by the runner's extension error, so it is only logged here.
pub fn report_extension_handler_stall(
    stall: &ExtensionHandlerStall,
    logger: &SessionLogger,
    notify: impl FnOnce(&str, &str),
) {
    logger.warn(
        "extension_handler_stalled",
        Some(&extension_handler_stall_log_fields(stall)),
    );
    if let Some(notice) = extension_handler_stall_notice(stall) {
        notify(&notice, "warning");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stall(stage: ExtensionHandlerStallStage, timeout_ms: u64) -> ExtensionHandlerStall {
        ExtensionHandlerStall {
            extension_path: "/ext/slow.ts".into(),
            event: "before_agent_start".into(),
            stage,
            elapsed_ms: 400,
            timeout_ms,
            error: None,
        }
    }

    #[test]
    fn the_warning_names_the_extension_and_its_cap() {
        assert_eq!(
            extension_handler_stall_notice(&stall(ExtensionHandlerStallStage::Warn, 30_000))
                .as_deref(),
            Some(
                "Extension slow.ts is still running its before_agent_start handler after 1 s, so this turn is waiting on it. The session continues without it after 30 s."
            )
        );
        assert!(
            extension_handler_stall_notice(&stall(ExtensionHandlerStallStage::Warn, 0))
                .is_some_and(|notice| notice.ends_with("Action: set extensionEventHandlerTimeoutMs to bound it, or remove the extension."))
        );
    }

    #[test]
    fn later_stages_are_only_logged() {
        let mut notified = false;
        report_extension_handler_stall(
            &stall(ExtensionHandlerStallStage::Timeout, 1),
            &SessionLogger::default(),
            |_, _| notified = true,
        );
        assert!(!notified);
        let fields = extension_handler_stall_log_fields(&ExtensionHandlerStall {
            error: Some("boom".into()),
            ..stall(ExtensionHandlerStallStage::LateFailure, 1)
        });
        assert_eq!(fields["stage"], "late_failure");
        assert_eq!(fields["error"], "boom");
        assert_eq!(fields["extension"], "slow.ts");
    }
}
