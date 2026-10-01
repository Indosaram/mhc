use crate::types::{HookDiagnostic, HookSourceMetadata, Severity};

pub struct DiagnosticDraft<'a> {
    pub code: &'a str,
    pub message: String,
    pub path: String,
    pub event: Option<&'a str>,
    pub severity: Option<Severity>,
}

pub fn diagnostic(draft: DiagnosticDraft<'_>, source: &HookSourceMetadata) -> HookDiagnostic {
    HookDiagnostic {
        code: draft.code.to_owned(),
        message: draft.message,
        path: draft.path,
        severity: draft.severity.unwrap_or(Severity::Error),
        source: source.clone(),
        event: draft.event.map(str::to_owned),
    }
}
