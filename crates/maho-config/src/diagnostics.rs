//! Load diagnostics and their human rendering (spec §10).

use crate::env::{forward_slashes, has_drive_prefix};
use std::path::{Path, PathBuf};

/// Diagnostic kinds (internal identifiers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    Read,
    Parse,
    Validation,
    UnknownKeys,
    InvalidValue,
    DeprecatedKeys,
    Profile,
    CatalogCycle,
}

impl DiagnosticKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticKind::Read => "read",
            DiagnosticKind::Parse => "parse",
            DiagnosticKind::Validation => "validation",
            DiagnosticKind::UnknownKeys => "unknown-keys",
            DiagnosticKind::InvalidValue => "invalid-value",
            DiagnosticKind::DeprecatedKeys => "deprecated-keys",
            DiagnosticKind::Profile => "profile",
            DiagnosticKind::CatalogCycle => "catalog-cycle",
        }
    }
}

/// What a diagnostic is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticTarget {
    /// A configuration file.
    File(PathBuf),
    /// The merged configuration (rendered `merged config`).
    Merged,
    /// A dotted configuration path (profile and catalog kinds).
    Config(String),
}

/// Rendering of [`DiagnosticTarget::Merged`].
pub const MERGED_CONFIG: &str = "merged config";

/// One structured note produced while loading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub target: DiagnosticTarget,
    pub message: String,
    /// Dotted issue paths (keys) the diagnostic is about.
    pub issues: Vec<String>,
}

impl Diagnostic {
    pub fn new(
        kind: DiagnosticKind,
        target: DiagnosticTarget,
        message: impl Into<String>,
        issues: Vec<String>,
    ) -> Self {
        Self {
            kind,
            target,
            message: message.into(),
            issues,
        }
    }

    /// The file this diagnostic is about, if any.
    pub fn file(&self) -> Option<&Path> {
        match &self.target {
            DiagnosticTarget::File(path) => Some(path),
            _ => None,
        }
    }
}

/// Renders a path for humans: `~/…` under `home`, unchanged elsewhere.
pub fn display_path(path: &Path, home: &Path) -> String {
    let text = forward_slashes(&path.to_string_lossy());
    let home_text = forward_slashes(&home.to_string_lossy());
    let home_text = home_text.trim_end_matches('/');
    if home_text.is_empty() {
        return text;
    }
    let case_insensitive = has_drive_prefix(home_text) || cfg!(windows);
    let (cmp_text, cmp_home) = if case_insensitive {
        (text.to_lowercase(), home_text.to_lowercase())
    } else {
        (text.clone(), home_text.to_owned())
    };
    if cmp_text == cmp_home {
        return "~".into();
    }
    if cmp_text.starts_with(&cmp_home) && cmp_text.as_bytes().get(cmp_home.len()) == Some(&b'/') {
        // Slicing by byte length is safe: lower-casing ASCII drive paths keeps lengths, and for
        // non-ASCII homes the case-sensitive comparison is used on Unix.
        if let Some(rest) = text.get(home_text.len()..) {
            return format!("~{rest}");
        }
    }
    text
}

/// Renders a diagnostic target for humans.
pub fn display_target(target: &DiagnosticTarget, home: &Path) -> String {
    match target {
        DiagnosticTarget::File(path) => display_path(path, home),
        DiagnosticTarget::Merged => MERGED_CONFIG.into(),
        DiagnosticTarget::Config(path) => path.clone(),
    }
}

/// Prefix of every doctor line.
pub const DOCTOR_PREFIX: &str = "config:";

/// Doctor report lines (without severity prefix).
pub fn doctor_lines(diagnostics: &[Diagnostic], home: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for diagnostic in diagnostics {
        let shown = display_target(&diagnostic.target, home);
        if let DiagnosticTarget::Config(path) = &diagnostic.target {
            if matches!(
                diagnostic.kind,
                DiagnosticKind::Validation | DiagnosticKind::CatalogCycle
            ) {
                lines.push(format!("{DOCTOR_PREFIX} {path}: {}", diagnostic.message));
            }
            continue;
        }
        match diagnostic.kind {
            DiagnosticKind::InvalidValue => {
                for key in &diagnostic.issues {
                    lines.push(format!(
                        "{DOCTOR_PREFIX} {shown}: {key} ignored (invalid value)"
                    ));
                }
            }
            DiagnosticKind::UnknownKeys => {
                for key in &diagnostic.issues {
                    lines.push(format!(
                        "{DOCTOR_PREFIX} {shown}: {key} ignored (unknown key)"
                    ));
                }
            }
            DiagnosticKind::Read | DiagnosticKind::Parse | DiagnosticKind::Validation => {
                let reason = match diagnostic.kind {
                    DiagnosticKind::Parse => "JSONC parse error".to_owned(),
                    DiagnosticKind::Read => "unreadable".to_owned(),
                    _ if diagnostic.issues.is_empty() => "invalid config".to_owned(),
                    _ => format!("invalid: {}", diagnostic.issues.join(", ")),
                };
                if diagnostic.target == DiagnosticTarget::Merged {
                    lines.push(format!(
                        "{DOCTOR_PREFIX} {MERGED_CONFIG}: reset to defaults ({reason})"
                    ));
                } else {
                    lines.push(format!("{DOCTOR_PREFIX} {shown}: not loaded ({reason})"));
                }
            }
            DiagnosticKind::CatalogCycle
            | DiagnosticKind::DeprecatedKeys
            | DiagnosticKind::Profile => {}
        }
    }
    lines
}
