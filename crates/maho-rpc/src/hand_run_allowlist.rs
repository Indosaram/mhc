//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-run-allowlist.ts`.
//!
//! The `run` operation's allowlist (S40 PR 2, `senpi#5645`, design v2.2-v2.4). A hand runs only
//! the `git` and `lsof` calls the memory component makes; everything else is refused with a reason
//! naming what was refused. The hand prepends its own git globals, builds the environment from
//! scratch, and never runs a shell.

use std::collections::{HashMap, HashSet};

use crate::hand_run_grammar::{TypedPath, match_shape};

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub exe: String,
    pub argv: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct HandRunContext {
    pub uid: i64,
    /// Node-style platform identifier (`"linux"`, `"darwin"`, `"win32"`, etc.).
    pub platform: String,
    /// The hand's own environment, read for the baseline only; the caller never supplies these.
    pub own_env: HashMap<String, String>,
}

impl HandRunContext {
    pub fn current() -> Self {
        #[cfg(unix)]
        let uid = i64::from(rustix::process::getuid().as_raw());
        #[cfg(not(unix))]
        let uid = -1;
        let platform = if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(windows) {
            "win32"
        } else {
            "linux"
        };
        Self {
            uid,
            platform: platform.to_owned(),
            own_env: std::env::vars().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedExe {
    Git,
    Lsof,
}

impl AllowedExe {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Lsof => "lsof",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPlan {
    Ok {
        exe: AllowedExe,
        argv: Vec<String>,
        env: HashMap<String, String>,
        /// Path arguments the hand confines on the device before spawning (none for `lsof`).
        paths: Vec<TypedPath>,
    },
    Refused {
        reason: String,
    },
}

const HAND_GIT_PREFIX: [&str; 4] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.file.allow=always",
];

const SUBCOMMANDS: [&str; 38] = [
    "add",
    "branch",
    "cat-file",
    "checkout",
    "commit",
    "commit-tree",
    "config",
    "count-objects",
    "diff",
    "diff-files",
    "diff-tree",
    "for-each-ref",
    "hash-object",
    "init",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "maintenance",
    "merge",
    "merge-base",
    "prune-packed",
    "push",
    "read-tree",
    "remote",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "rm",
    "show",
    "show-ref",
    "status",
    "symbolic-ref",
    "update-index",
    "update-ref",
    "worktree",
    "write-tree",
];

/// Section and name compare case-insensitively, a subsection exactly, as git reads config keys.
fn config_key(key: &str) -> String {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() < 2 {
        return key.to_owned();
    }
    let section = parts[0].to_ascii_lowercase();
    let name = parts[parts.len() - 1].to_ascii_lowercase();
    let mut out = Vec::with_capacity(parts.len());
    out.push(section);
    for middle in &parts[1..parts.len() - 1] {
        out.push((*middle).to_owned());
    }
    out.push(name);
    out.join(".")
}

fn is_caller_c_key(key: &str) -> bool {
    let normalized = config_key(key);
    [
        "maintenance.auto",
        "gc.auto",
        "user.name",
        "user.email",
        "core.autocrlf",
        "log.showRoot",
        "core.quotePath",
        "log.showSignature",
        "log.follow",
    ]
    .iter()
    .any(|k| config_key(k) == normalized)
}

fn is_config_key(key: &str) -> bool {
    let normalized = config_key(key);
    [
        "omo.agentId",
        "user.name",
        "user.email",
        "commit.gpgsign",
        "gc.auto",
        "core.autocrlf",
        "omo.memoryRepository.url",
        "omo.sync.binding",
        "omo.sync.space",
    ]
    .iter()
    .any(|k| config_key(k) == normalized)
}

fn subcommand_args(sub: &str, rest: &[String]) -> Result<Vec<TypedPath>, String> {
    if sub == "config" {
        let scope = rest.first().map(String::as_str);
        let first = rest.get(1).map(String::as_str);
        let second = rest.get(2).map(String::as_str);
        if scope != Some("--local") || first.is_none() || rest.len() > 3 {
            return Err("config shape".to_owned());
        }
        let first_val = first.unwrap_or_default();
        let key = if first_val == "--get" || first_val == "--unset-all" {
            second
        } else {
            Some(first_val)
        };
        let Some(key) = key else {
            return Err("config shape".to_owned());
        };
        if first_val != "--get"
            && first_val != "--unset-all"
            && second.unwrap_or("-").starts_with('-')
        {
            return Err("config shape".to_owned());
        }
        return if is_config_key(key) {
            Ok(Vec::new())
        } else {
            Err(format!("config key {key}"))
        };
    }
    match_shape(sub, rest).ok_or_else(|| format!("{sub} shape"))
}

struct PlannedGit {
    argv: Vec<String>,
    paths: Vec<TypedPath>,
}

fn plan_git(argv: &[String]) -> Result<PlannedGit, String> {
    let mut index = 0usize;
    while let Some(arg) = argv.get(index)
        && arg.starts_with('-')
    {
        if arg == "--literal-pathspecs" {
            index += 1;
            continue;
        }
        if arg != "-c" {
            return Err(arg.clone());
        }
        let setting = argv.get(index + 1).map(String::as_str).unwrap_or("");
        let Some(separator) = setting.find('=') else {
            return Err(format!("-c {setting}"));
        };
        if separator == 0 || !is_caller_c_key(&setting[..separator]) {
            return Err(format!("-c {setting}"));
        }
        index += 2;
    }
    let Some(sub) = argv.get(index) else {
        return Err("subcommand (none)".to_owned());
    };
    let subs: HashSet<&str> = SUBCOMMANDS.iter().copied().collect();
    if !subs.contains(sub.as_str()) {
        return Err(format!("subcommand {sub}"));
    }
    let paths = subcommand_args(sub, &argv[index + 1..])?;
    let mut out = Vec::with_capacity(HAND_GIT_PREFIX.len() + argv.len());
    out.extend(HAND_GIT_PREFIX.iter().map(|s| (*s).to_owned()));
    out.extend(argv.iter().cloned());
    Ok(PlannedGit { argv: out, paths })
}

fn plan_lsof(argv: &[String], uid: i64) -> Result<Vec<String>, String> {
    let holders = argv.len() == 3 && argv[0] == "-t" && argv[1] == "--" && !argv[2].is_empty();
    let uid_str = uid.to_string();
    let expected = ["-w", "-a", "-d", "cwd", "-u", uid_str.as_str(), "-Fpn"];
    let in_use = argv.len() == expected.len() && argv.iter().zip(expected).all(|(a, b)| a == b);
    if holders || in_use {
        Ok(argv.to_vec())
    } else {
        Err("lsof shape".to_owned())
    }
}

fn caller_env_allowed(key: &str, value: &str) -> bool {
    match key {
        "GIT_TERMINAL_PROMPT" => value == "0",
        "GIT_INDEX_FILE" => !value.is_empty(),
        "GIT_GLOB_PATHSPECS" => value == "0" || value == "1",
        "GIT_AUTHOR_NAME" | "GIT_AUTHOR_EMAIL" | "GIT_COMMITTER_NAME" | "GIT_COMMITTER_EMAIL" => {
            true
        }
        _ => false,
    }
}

const WIN32_BASELINE: [&str; 7] = [
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "PATHEXT",
];

fn baseline(context: &HandRunContext) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let mut keep = |key: &str| {
        if let Some(val) = context.own_env.get(key) {
            env.insert(key.to_owned(), val.clone());
        }
    };
    for key in ["PATH", "HOME", "SSH_AUTH_SOCK"] {
        keep(key);
    }
    if context.platform == "win32" {
        for key in WIN32_BASELINE {
            keep(key);
        }
    } else {
        keep("TMPDIR");
        env.insert("LANG".to_owned(), "C".to_owned());
    }
    env.insert("GIT_PROTOCOL_FROM_USER".to_owned(), "0".to_owned());
    env
}

/// Turns a `run` request into the exact argv and environment the hand spawns, or a refusal naming
/// what was refused.
pub fn plan_run(request: &RunRequest, context: &HandRunContext) -> RunPlan {
    let mut env = baseline(context);
    for (key, value) in &request.env {
        if !caller_env_allowed(key, value) {
            return RunPlan::Refused {
                reason: format!("not in the run allowlist: env {key}"),
            };
        }
        env.insert(key.clone(), value.clone());
    }
    match request.exe.as_str() {
        "git" => match plan_git(&request.argv) {
            Ok(planned) => RunPlan::Ok {
                exe: AllowedExe::Git,
                argv: planned.argv,
                env,
                paths: planned.paths,
            },
            Err(reason) => RunPlan::Refused {
                reason: format!("not in the run allowlist: {reason}"),
            },
        },
        "lsof" => match plan_lsof(&request.argv, context.uid) {
            Ok(argv) => RunPlan::Ok {
                exe: AllowedExe::Lsof,
                argv,
                env,
                paths: Vec::new(),
            },
            Err(reason) => RunPlan::Refused {
                reason: format!("not in the run allowlist: {reason}"),
            },
        },
        other => RunPlan::Refused {
            reason: format!("not in the run allowlist: executable {other}"),
        },
    }
}

fn is_safe_token(arg: &str) -> bool {
    !arg.is_empty()
        && arg.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'_' | b'-'
                )
        })
}

/// The POSIX-quoted command line the hand's `tool_call` handlers judge: exactly what is spawned,
/// no shell involved.
pub fn render_command(exe: &str, argv: &[String]) -> String {
    std::iter::once(exe)
        .chain(argv.iter().map(String::as_str))
        .map(|arg| {
            if is_safe_token(arg) {
                arg.to_owned()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> HandRunContext {
        HandRunContext {
            uid: 1000,
            platform: "linux".to_owned(),
            own_env: HashMap::from([
                ("PATH".to_owned(), "/usr/bin".to_owned()),
                ("SECRET".to_owned(), "nope".to_owned()),
            ]),
        }
    }

    #[test]
    fn plans_allowed_git_and_lsof_and_builds_clean_env() {
        let plan = plan_run(
            &RunRequest {
                exe: "git".to_owned(),
                argv: vec![
                    "-c".to_owned(),
                    "User.Name=Alice".to_owned(),
                    "status".to_owned(),
                    "--porcelain".to_owned(),
                ],
                env: HashMap::from([("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned())]),
            },
            &ctx(),
        );
        match plan {
            RunPlan::Ok { exe, argv, env, .. } => {
                assert_eq!(exe, AllowedExe::Git);
                assert_eq!(&argv[..4], &HAND_GIT_PREFIX);
                assert_eq!(env.get("LANG").map(String::as_str), Some("C"));
                assert_eq!(
                    env.get("GIT_PROTOCOL_FROM_USER").map(String::as_str),
                    Some("0")
                );
                assert!(!env.contains_key("SECRET"));
            }
            RunPlan::Refused { reason } => panic!("unexpected refusal: {reason}"),
        }

        let lsof_ok = plan_run(
            &RunRequest {
                exe: "lsof".to_owned(),
                argv: vec![
                    "-w".into(),
                    "-a".into(),
                    "-d".into(),
                    "cwd".into(),
                    "-u".into(),
                    "1000".into(),
                    "-Fpn".into(),
                ],
                env: HashMap::new(),
            },
            &ctx(),
        );
        assert!(matches!(
            lsof_ok,
            RunPlan::Ok {
                exe: AllowedExe::Lsof,
                ..
            }
        ));
    }

    #[test]
    fn refuses_disallowed_exe_env_config_and_subcommands() {
        for req in [
            RunRequest {
                exe: "bash".into(),
                argv: vec!["-c".into(), "id".into()],
                env: HashMap::new(),
            },
            RunRequest {
                exe: "git".into(),
                argv: vec!["status".into()],
                env: HashMap::from([("LD_PRELOAD".into(), "/tmp/x.so".into())]),
            },
            RunRequest {
                exe: "git".into(),
                argv: vec!["clone".into(), "https://x".into()],
                env: HashMap::new(),
            },
            RunRequest {
                exe: "git".into(),
                argv: vec!["config".into(), "--global".into(), "user.name".into()],
                env: HashMap::new(),
            },
            RunRequest {
                exe: "lsof".into(),
                argv: vec!["-u".into(), "0".into()],
                env: HashMap::new(),
            },
        ] {
            assert!(matches!(plan_run(&req, &ctx()), RunPlan::Refused { .. }));
        }
    }

    #[test]
    fn render_command_quotes_unsafe_tokens() {
        assert_eq!(
            render_command("git", &["commit".into(), "-m".into(), "it's fine".into(),]),
            "git commit -m 'it'\\''s fine'"
        );
    }
}
