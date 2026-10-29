//! Main CLI command router for ulw-loop.

use std::env;
use std::path::{Path, PathBuf};

use crate::cli_arg_parser::{has_flag, read_value};
use crate::cli_output::{print_json_error, ULW_LOOP_HELP};
use crate::cli_subcommands::{
    add_goal, capture_evidence, checkpoint_cli, complete_goals, create_goals, criteria,
    review_blockers, status, steer,
};
use crate::paths::{resolve_ulw_loop_session_id_from_env, UlwLoopScope};
use crate::plan_io::list_ulw_loop_session_ids;
use crate::plan_missing_recovery::{session_id_required_message, session_scope_required_message};
use crate::runtime::UlwLoopError;

pub const ULW_LOOP_SUBCOMMANDS: [&str; 10] = [
    "help",
    "create-goals",
    "status",
    "complete-goals",
    "checkpoint",
    "steer",
    "add-goal",
    "criteria",
    "record-evidence",
    "record-review-blockers",
];

pub fn is_ulw_loop_subcommand(value: &str) -> bool {
    ULW_LOOP_SUBCOMMANDS.contains(&value)
}

const SESSION_ID_FLAG: &str = "--session-id";

fn session_id_flag_present(argv: &[String]) -> bool {
    has_flag(argv, SESSION_ID_FLAG) || argv.iter().any(|arg| arg.starts_with(&format!("{SESSION_ID_FLAG}=")))
}

fn command_scope(repo_root: &Path, argv: &[String]) -> Result<UlwLoopScope, UlwLoopError> {
    if session_id_flag_present(argv) {
        let session_id = read_value(argv, SESSION_ID_FLAG);
        let trimmed = session_id.as_deref().unwrap_or("").trim().to_string();
        if trimmed.is_empty() {
            return Err(UlwLoopError::new(
                session_id_required_message(SESSION_ID_FLAG),
                "ULW_LOOP_SESSION_ID_REQUIRED",
            ));
        }
        return Ok(UlwLoopScope {
            session_id: Some(trimmed),
            ..Default::default()
        });
    }

    if let Some(session_id) = resolve_ulw_loop_session_id_from_env(&env::vars().collect()) {
        return Ok(UlwLoopScope {
            session_id: Some(session_id),
            ..Default::default()
        });
    }

    let existing = list_ulw_loop_session_ids(repo_root);
    Err(UlwLoopError::new(
        session_scope_required_message(SESSION_ID_FLAG, &existing),
        "ULW_LOOP_SESSION_SCOPE_REQUIRED",
    ))
}

pub fn ulw_loop_command(argv: &[String]) -> i32 {
    let head = argv.first().map(|s| s.as_str()).unwrap_or("help");
    let command = if head == "--help" || head == "-h" {
        "help"
    } else {
        head
    };
    let rest = if argv.is_empty() { &[] } else { &argv[1..] };
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let json = has_flag(rest, "--json");

    if !is_ulw_loop_subcommand(command) {
        if json {
            print_json_error(&UlwLoopError::new(
                format!("Unknown ulw-loop subcommand: {command}."),
                "ULW_LOOP_SUBCOMMAND_UNKNOWN",
            ));
            return 1;
        }
        println!("{ULW_LOOP_HELP}");
        return 1;
    }

    if command == "help" {
        println!("{ULW_LOOP_HELP}");
        return 0;
    }

    let scope = match command_scope(&cwd, rest) {
        Ok(s) => s,
        Err(err) => {
            if json {
                print_json_error(&err);
            } else {
                eprintln!("[ulw-loop] {}", err.message);
            }
            return 1;
        }
    };

    let result = match command {
        "create-goals" => create_goals(&cwd, rest, json, Some(&scope)),
        "status" => status(&cwd, json, Some(&scope)),
        "complete-goals" => complete_goals(&cwd, rest, json, Some(&scope)),
        "checkpoint" => checkpoint_cli(&cwd, rest, json, Some(&scope)),
        "steer" => steer(&cwd, rest, json, Some(&scope)),
        "add-goal" => add_goal(&cwd, rest, json, Some(&scope)),
        "criteria" => criteria(&cwd, rest, json, Some(&scope)),
        "record-evidence" => capture_evidence(&cwd, rest, json, Some(&scope)),
        "record-review-blockers" => review_blockers(&cwd, rest, json, Some(&scope)),
        _ => Err(UlwLoopError::new(
            format!("Unhandled ulw-loop subcommand: {command}."),
            "ULW_LOOP_SUBCOMMAND_UNHANDLED",
        )),
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            if json {
                print_json_error(&err);
            } else {
                eprintln!("[ulw-loop] {}", err.message);
            }
            1
        }
    }
}
