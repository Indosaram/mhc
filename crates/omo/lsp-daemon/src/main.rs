//! `lsp-daemon` CLI (port of `cli.ts`): `lsp-daemon [mcp | daemon]`, default `mcp`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_else(|| "mcp".to_string());
    let result = match command.as_str() {
        "daemon" => run_daemon(),
        "mcp" => lsp_daemon::proxy::run_process_proxy()
            .map(|_outcome| ())
            .map_err(|error| error.to_string()),
        _ => {
            eprintln!("Usage: omo-lsp-daemon [mcp | daemon]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run_daemon() -> Result<(), String> {
    let paths = lsp_daemon::paths::daemon_paths().map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(lsp_daemon::daemon_server::run_daemon(&paths))
        .map_err(startup_error_message)
}

fn startup_error_message(error: lsp_daemon::ownership::StartupError) -> String {
    match error {
        lsp_daemon::ownership::StartupError::Deferred(reason) => {
            format!("[lsp-daemon] startup deferred: {reason}")
        }
        lsp_daemon::ownership::StartupError::AlreadyRunning => error.to_string(),
        lsp_daemon::ownership::StartupError::Io(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_daemon::ownership::StartupError;

    #[test]
    fn deferred_startup_is_one_line() {
        let error = StartupError::Deferred("startup_lock_busy");
        let message = startup_error_message(error);
        assert_eq!(message, "[lsp-daemon] startup deferred: startup_lock_busy");
        assert_eq!(message.lines().count(), 1);
    }

    #[test]
    fn unexpected_error_keeps_its_message() {
        let error = std::io::Error::other("unexpected startup failure");
        assert_eq!(startup_error_message(StartupError::Io(error)), "unexpected startup failure");
    }
}
