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
        .map_err(|error| error.to_string())
}
