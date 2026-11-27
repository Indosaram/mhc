//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-run-transport.ts`.
//!
//! The transport guards of a `run` git call (S40 PR 2, design v3.1, `senpi#5645`). They are part
//! of the argv the hand's handlers judge, so the judged command is exactly the one that runs; a
//! caller can never supply or omit them.

use std::collections::HashMap;

use crate::hand_run_grammar::{TypedPath, TypedPathKind};

/// The receive-pack every LOCAL push runs (V3). Its `-c` settings reach the local receive-pack and
/// its children: no target or global hook runs, a checked-out branch is refused, and no auto gc or
/// maintenance can prune through the target.
const HARDENED_RECEIVE_PACK: &str = "git \
-c core.hooksPath=/dev/null \
-c receive.denyCurrentBranch=refuse \
-c core.fsmonitor=false \
-c receive.autogc=false \
-c gc.auto=0 \
-c maintenance.auto=false \
receive-pack";

/// A LOCAL run runs no client hook either (`pre-push`, `reference-transaction`): the hand, not the
/// repo, decides what executes.
const CLIENT_LOCAL_GUARDS: [&str; 2] = ["-c", "core.hooksPath=/dev/null"];

/// A REMOTE run may not fall back to a local transport through `insteadOf`, `pushInsteadOf` or a
/// remote name (V6).
const REMOTE_GUARDS: [&str; 6] = [
    "-c",
    "protocol.file.allow=never",
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "protocol.fd.allow=never",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTransport {
    pub argv: Vec<String>,
    pub env: HashMap<String, String>,
    /// A local run, whose effective config must then carry no `url.*` rewrite (V5).
    pub local: bool,
}

fn subcommand_at(argv: &[String]) -> usize {
    let mut index = 0;
    while index < argv.len() && argv[index].starts_with('-') {
        index += if argv[index] == "-c" { 2 } else { 1 };
    }
    index
}

/// The final argv and environment for a planned git call: local and remote transports each get their
/// own guards.
pub fn transport_for(
    argv: &[String],
    env: &HashMap<String, String>,
    paths: &[TypedPath],
) -> RunTransport {
    let at = subcommand_at(argv);
    let sub = argv.get(at).map(String::as_str);
    let local = paths
        .iter()
        .any(|p| matches!(p.kind, TypedPathKind::Source | TypedPathKind::Target));
    if local {
        let mut out_argv = Vec::with_capacity(argv.len() + 3);
        out_argv.extend_from_slice(&argv[..at]);
        out_argv.extend(CLIENT_LOCAL_GUARDS.iter().map(|s| (*s).to_owned()));
        out_argv.push(sub.unwrap_or("").to_owned());
        if sub == Some("push") {
            out_argv.push(format!("--receive-pack={HARDENED_RECEIVE_PACK}"));
        }
        if at < argv.len() {
            out_argv.extend_from_slice(&argv[at + 1..]);
        }
        let mut out_env = env.clone();
        out_env.insert("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned());
        out_env.insert("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned());
        return RunTransport {
            argv: out_argv,
            env: out_env,
            local: true,
        };
    }
    if matches!(sub, Some("ls-remote" | "push")) {
        let mut out_argv = Vec::with_capacity(argv.len() + REMOTE_GUARDS.len());
        out_argv.extend_from_slice(&argv[..at]);
        out_argv.extend(REMOTE_GUARDS.iter().map(|s| (*s).to_owned()));
        out_argv.extend_from_slice(&argv[at..]);
        return RunTransport {
            argv: out_argv,
            env: env.clone(),
            local: false,
        };
    }
    RunTransport {
        argv: argv.to_vec(),
        env: env.clone(),
        local: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_hardened_receive_pack_and_nosystem_for_local_push() {
        let argv = vec![
            "-c".into(),
            "core.fsmonitor=false".into(),
            "push".into(),
            "--quiet".into(),
            "file:///tmp/repo".into(),
            "main:main".into(),
        ];
        let paths = vec![TypedPath {
            kind: TypedPathKind::Target,
            value: "file:///tmp/repo".into(),
        }];
        let t = transport_for(&argv, &HashMap::new(), &paths);
        assert!(t.local);
        assert_eq!(
            t.env.get("GIT_CONFIG_GLOBAL").map(String::as_str),
            Some("/dev/null")
        );
        assert!(
            t.argv
                .iter()
                .any(|a| a.starts_with("--receive-pack=git -c core.hooksPath=/dev/null"))
        );
        assert!(
            t.argv
                .windows(2)
                .any(|w| w == ["-c", "core.hooksPath=/dev/null"])
        );
    }

    #[test]
    fn adds_remote_protocol_guards_for_remote_push_and_ls_remote() {
        let argv = vec![
            "ls-remote".into(),
            "https://example.com/a.git".into(),
            "HEAD".into(),
        ];
        let t = transport_for(&argv, &HashMap::new(), &[]);
        assert!(!t.local);
        assert!(
            t.argv
                .windows(2)
                .any(|w| w == ["-c", "protocol.file.allow=never"])
        );
        assert!(
            t.argv
                .windows(2)
                .any(|w| w == ["-c", "protocol.ext.allow=never"])
        );
        assert!(
            t.argv
                .windows(2)
                .any(|w| w == ["-c", "protocol.fd.allow=never"])
        );
    }
}
