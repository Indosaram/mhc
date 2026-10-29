//! Main entry point for ulw-loop command-line interface.

use std::env;
use std::io::{self, Write};

use crate::cli_commands::ulw_loop_command;
use crate::cli_output::ULW_LOOP_HELP;
use crate::codex_hook::{run_pre_tool_use_goal_budget_guard_cli, run_ulw_loop_hook_cli, UserPromptSubmitHookOptions};
use crate::spawn_guard::{run_spawn_admission_recorder_cli, run_spawn_guard_cli};
use crate::stop_resume_hook::run_stop_resume_hook;

pub fn main_cli(args: &[String]) -> i32 {
    let command = args.first().map(|s| s.as_str());
    match command {
        None | Some("help") | Some("--help") | Some("-h") => {
            println!("{ULW_LOOP_HELP}");
            0
        }
        Some("ulw-loop") => ulw_loop_command(&args[1..]),
        Some("hook") => {
            let sub = args.get(1).map(|s| s.as_str());
            match sub {
                Some("user-prompt-submit") => {
                    let with_ulw = args.iter().any(|a| a == "--with-ultrawork");
                    let options = UserPromptSubmitHookOptions {
                        include_ultrawork_directive: with_ulw,
                        ultrawork_skill_file_path: None,
                    };
                    run_ulw_loop_hook_cli(io::stdin(), io::stdout(), &options);
                    0
                }
                Some("pre-tool-use") => {
                    run_pre_tool_use_goal_budget_guard_cli(io::stdin(), io::stdout());
                    0
                }
                Some("stop-resume") => {
                    let mut raw = String::new();
                    let _ = io::Read::read_to_string(&mut io::stdin(), &mut raw);
                    if let Ok(val) = serde_json::from_str(&raw) {
                        let out = run_stop_resume_hook(&val);
                        if !out.is_empty() {
                            let _ = io::stdout().write_all(out.as_bytes());
                        }
                    }
                    0
                }
                Some("spawn-guard") => {
                    run_spawn_guard_cli(io::stdin(), io::stdout());
                    0
                }
                Some("spawn-admission-recorder") => {
                    run_spawn_admission_recorder_cli(io::stdin(), io::stdout());
                    0
                }
                _ => 1,
            }
        }
        _ => 1,
    }
}
