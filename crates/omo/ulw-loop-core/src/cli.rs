//! Main entry point for ulw-loop command-line interface.

use crate::cli_commands::ulw_loop_command;
use crate::cli_output::ULW_LOOP_HELP;

pub fn main_cli(args: &[String]) -> i32 {
    let command = args.first().map(|s| s.as_str());
    match command {
        None | Some("help") | Some("--help") | Some("-h") => {
            println!("{ULW_LOOP_HELP}");
            0
        }
        Some("ulw-loop") => ulw_loop_command(&args[1..]),
        _ => 1,
    }
}
