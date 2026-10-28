//! `mhc home [show|init|repair] [--json]` (spec §5).

use crate::{AgentHome, Bootstrapped};

pub const USAGE: &str = "Usage: mhc home [show] [--json]
       mhc home init
       mhc home repair

show    list the agent home layout (default); exit 1 when an entry has a problem
init    create the missing directories
repair  create the missing directories and make auth.json private";

/// Output of one command run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

fn usage_error(message: &str) -> Output {
    Output {
        stderr: format!("mhc home: {message}\n{USAGE}"),
        code: 2,
        ..Output::default()
    }
}

/// Runs `mhc home <args>` against `home`.
pub fn run(args: &[String], home: &AgentHome) -> Output {
    let mut command = None;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                return Output {
                    stdout: USAGE.to_owned(),
                    ..Output::default()
                };
            }
            "--json" => json = true,
            "show" | "init" | "repair" if command.is_none() => command = Some(arg.as_str()),
            other => return usage_error(&format!("unknown argument \"{other}\"")),
        }
    }
    match command.unwrap_or("show") {
        "init" => match home.bootstrap() {
            Ok(done) => {
                let mut lines: Vec<String> = done
                    .iter()
                    .filter(|(_, b)| *b == Bootstrapped::Created)
                    .map(|(entry, _)| format!("created {}", entry.path.display()))
                    .collect();
                lines.push(format!("agent home ready: {}", home.root().display()));
                Output {
                    stdout: lines.join("\n"),
                    ..Output::default()
                }
            }
            Err(error) => Output {
                stderr: format!("mhc home: {error}"),
                code: 1,
                ..Output::default()
            },
        },
        "repair" => match home.repair() {
            Ok(repaired) => {
                let mut lines: Vec<String> = repaired
                    .iter()
                    .map(|path| format!("made private {}", path.display()))
                    .collect();
                lines.push(format!("agent home ready: {}", home.root().display()));
                Output {
                    stdout: lines.join("\n"),
                    ..Output::default()
                }
            }
            Err(error) => Output {
                stderr: format!("mhc home: {error}"),
                code: 1,
                ..Output::default()
            },
        },
        _ => show(home, json),
    }
}

fn show(home: &AgentHome, json: bool) -> Output {
    let inspected = home.inspect();
    let failed = inspected.iter().any(|i| i.problem.is_some());
    let stdout = if json {
        home.report_json().to_string()
    } else {
        let width = inspected
            .iter()
            .map(|i| i.entry.name.len())
            .max()
            .unwrap_or(0);
        let mut lines: Vec<String> = inspected
            .iter()
            .map(|i| {
                let state = match (&i.problem, i.exists) {
                    (Some(problem), _) => format!("PROBLEM {problem}"),
                    (None, true) => "ok".to_owned(),
                    (None, false) => "missing".to_owned(),
                };
                format!(
                    "{:width$}  {}  ({state})",
                    i.entry.name,
                    i.entry.path.display()
                )
            })
            .collect();
        lines.push(format!("setup version: {}", home.setup_version()));
        lines.join("\n")
    };
    Output {
        stdout,
        code: i32::from(failed),
        ..Output::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn init_then_show_reports_ready_layout() {
        let dir = tempfile::tempdir().expect("temp");
        let home = AgentHome::new(dir.path(), &dir.path().join(".omo/agent"));
        let init = run(&args(&["init"]), &home);
        assert_eq!(init.code, 0);
        assert!(init.stdout.contains("created "));
        assert!(
            init.stdout
                .ends_with(&format!("agent home ready: {}", home.root().display()))
        );
        let show = run(&[], &home);
        assert_eq!(show.code, 0, "{}", show.stdout);
        assert!(show.stdout.contains("sessions"));
        assert!(show.stdout.contains("(missing)"));
        assert!(show.stdout.ends_with("setup version: 0"));
        let json = run(&args(&["show", "--json"]), &home);
        let value: serde_json::Value = serde_json::from_str(&json.stdout).expect("json");
        assert_eq!(value["entries"].as_array().map(Vec::len), Some(13));
    }

    #[test]
    fn a_problem_makes_show_exit_one() {
        let dir = tempfile::tempdir().expect("temp");
        let home = AgentHome::new(dir.path(), &dir.path().join(".omo/agent"));
        std::fs::create_dir_all(home.agent_dir()).expect("agent");
        std::fs::write(home.agent_dir().join("bin"), "").expect("file");
        let show = run(&[], &home);
        assert_eq!(show.code, 1);
        assert!(show.stdout.contains("PROBLEM wrong kind"));
    }

    #[test]
    fn unknown_arguments_and_help() {
        let dir = tempfile::tempdir().expect("temp");
        let home = AgentHome::new(dir.path(), &dir.path().join(".omo/agent"));
        let bad = run(&args(&["frobnicate"]), &home);
        assert_eq!(bad.code, 2);
        assert!(bad.stderr.contains("unknown argument \"frobnicate\""));
        let help = run(&args(&["--help"]), &home);
        assert_eq!(help.code, 0);
        assert!(help.stdout.starts_with("Usage: mhc home"));
    }
}
