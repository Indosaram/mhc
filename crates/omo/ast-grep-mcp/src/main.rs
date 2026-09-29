use std::process::ExitCode;

use ast_grep_mcp::AstGrepMcpOptions;
use ast_grep_mcp::run_mcp_stdio_server;

fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_else(|| "mcp".to_owned());
    if command != "mcp" {
        eprintln!("Usage: omo-ast-grep [mcp]");
        return ExitCode::from(2);
    }
    let mut stdout = std::io::stdout();
    match run_mcp_stdio_server(std::io::stdin(), &mut stdout, AstGrepMcpOptions::default()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}
