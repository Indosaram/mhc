use super::*;
use pretty_assertions::assert_eq;

fn exited(server_id: &str, stderr: &str) -> LspError {
    LspError::ProcessExited {
        server_id: server_id.to_string(),
        root: "/repo".to_string(),
        exit_code: Some(1),
        stderr_tail: Some(stderr.to_string()),
    }
}

#[test]
fn rust_src_component_conflict_returns_repair_guidance() {
    let error = exited(
        "rust",
        "failed to install component: 'rust-src', detected conflict: 'lib/rustlib/src/rust/library/Cargo.lock'",
    );

    let message = format_known_lsp_startup_failure(&error).expect("guidance");

    for expected in [
        "rust-analyzer",
        "rustup component remove rust-src",
        "rustup component add rust-src",
        "detected conflict",
        "Cargo.lock",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    assert!(!message.contains("automatic repair"));
}

#[test]
fn rust_analyzer_sysroot_error_returns_repair_guidance_via_missing_dependency() {
    let error = exited(
        "rust",
        "can't load standard library from sysroot\ntry installing `rust-src` the same way you installed `rustc`",
    );

    let message = handle_missing_dependency_error(&error).expect("guidance");

    for expected in [
        "rustup component remove rust-src",
        "rustup component add rust-src",
        "can't load standard library",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
}

#[test]
fn unrelated_process_exits_return_none() {
    let typescript = exited(
        "typescript",
        "failed to install component: 'rust-src', detected conflict",
    );
    let rust_panic = exited("rust", "thread panicked while loading crate graph");

    assert_eq!(
        (
            format_known_lsp_startup_failure(&typescript),
            format_known_lsp_startup_failure(&rust_panic)
        ),
        (None, None)
    );
}

#[test]
fn existing_dependency_messages_are_preserved() {
    let not_installed = LspError::other("LSP server 'typescript' is configured but NOT INSTALLED.");
    let not_configured = LspError::other("No LSP server configured for extension: .md");

    assert_eq!(
        (
            handle_missing_dependency_error(&not_installed),
            handle_missing_dependency_error(&not_configured)
        ),
        (
            Some("LSP server 'typescript' is configured but NOT INSTALLED.".to_string()),
            Some("No LSP server configured for extension: .md".to_string()),
        )
    );
}
