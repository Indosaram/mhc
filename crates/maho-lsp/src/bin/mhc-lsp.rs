//! `mhc-lsp`: the shared language-server mux daemon used by mhc.
//!
//! `mhc-lsp serve [--socket PATH]` listens until idle; `mhc-lsp ping [--socket PATH]` exits 0
//! when a daemon answers; `mhc-lsp socket` prints the default socket path.

#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    use maho_lsp::daemon;
    use std::process::ExitCode;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => {
            let socket = daemon::socket_from_args(&args[1..]);
            let banner = daemon::ready_banner(&socket);
            match daemon::serve(&socket, daemon::DaemonOptions::default(), move || {
                println!("{banner}")
            })
            .await
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("mhc-lsp: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("ping") => {
            if daemon::ping(&daemon::socket_from_args(&args[1..])).await {
                println!("{}", daemon::PING_RESULT);
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("socket") => {
            println!("{}", daemon::default_socket_path().display());
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("mhc-lsp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "usage: mhc-lsp serve [--socket PATH] | ping [--socket PATH] | socket | --version"
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "mhc-lsp: the daemon is only available on unix; mhc spawns language servers directly here"
    );
    std::process::ExitCode::FAILURE
}
