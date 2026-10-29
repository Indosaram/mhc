use crate::lsp::errors::LspError;

const RUST_SRC_REPAIR_MESSAGE: [&str; 5] = [
    "rust-analyzer exited while loading Rust standard library sources.",
    "",
    "Repair rust-src for the active toolchain:",
    "  rustup component remove rust-src",
    "  rustup component add rust-src",
];

/// TS `errorMessage`.
pub fn error_message(error: &dyn std::fmt::Display) -> String {
    error.to_string()
}

/// TS `formatKnownLspStartupFailure`: repair guidance for rust-analyzer rust-src failures.
pub fn format_known_lsp_startup_failure(error: &LspError) -> Option<String> {
    let LspError::ProcessExited {
        server_id,
        stderr_tail,
        ..
    } = error
    else {
        return None;
    };
    if server_id != "rust" {
        return None;
    }
    let details = stderr_tail.clone().unwrap_or_else(|| error.to_string());
    let lower = details.to_lowercase();
    let is_rust_src_failure = lower.contains("rust-src")
        && (lower.contains("failed to install component")
            || lower.contains("detected conflict")
            || lower.contains("can't load standard library")
            || lower.contains("try installing")
            || lower.contains("sysroot"));
    if !is_rust_src_failure {
        return None;
    }
    let mut lines: Vec<String> = RUST_SRC_REPAIR_MESSAGE
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    lines.push(String::new());
    lines.push("Original stderr tail:".to_string());
    lines.push(details);
    Some(lines.join("\n"))
}

/// TS `handleMissingDependencyError`.
pub fn handle_missing_dependency_error(error: &LspError) -> Option<String> {
    if let Some(known) = format_known_lsp_startup_failure(error) {
        return Some(known);
    }
    let message = error.to_string();
    (message.contains("NOT INSTALLED") || message.contains("No LSP server configured"))
        .then_some(message)
}

#[cfg(test)]
#[path = "utils_tests.rs"]
mod tests;
