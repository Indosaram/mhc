//! Port of `packages/git-bash-mcp/src/cli.ts` — bin `omo-git-bash`.

use std::process::ExitCode;

use crate::mcp::GitBashMcpOptions;
use crate::mcp::run_mcp_stdio_server;

/// Runs the `omo-git-bash` CLI: `mcp` (the default subcommand) serves stdio MCP, anything else is
/// a usage error with exit code 2.
pub fn run(args: &[String]) -> ExitCode {
    let command = args.first().map(String::as_str).unwrap_or("mcp");
    if command != "mcp" {
        eprintln!("Usage: omo-git-bash [mcp]");
        return ExitCode::from(2);
    }
    let mut stdout = std::io::stdout();
    match run_mcp_stdio_server(std::io::stdin(), &mut stdout, GitBashMcpOptions::default()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}
