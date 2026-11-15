//! Port of senpi `components/config-startup/metadata-launch.ts`.
//!
//! Senpi loads every extension even for launches that only print metadata: `--help` loads them to
//! list their flags, and `--list-models` / `--list-tips` build the runtime and exit before any
//! session. The OpenCode config migration belongs to the first real session, so these launches must
//! leave the user's files alone. The walk mirrors senpi's own argv parser: an option's value is
//! never read as an option, a multi-session host serves sessions whatever else it was given, and
//! everything after `--` is message text.

use std::collections::BTreeSet;
use std::sync::LazyLock;

const METADATA_OPTIONS: [&str; 4] = ["--help", "-h", "--list-models", "--list-tips"];
const SESSION_HOST_OPTIONS: [&str; 2] = ["--multi-session", "--listen"];

static ALWAYS_TAKES_VALUE: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "--provider",
        "--model",
        "--api-key",
        "--system-prompt",
        "--append-system-prompt",
        "--name",
        "-n",
        "--session",
        "--session-id",
        "--fork",
        "--rebind",
        "--session-dir",
        "--models",
        "--tools",
        "-t",
        "--exclude-tools",
        "-xt",
        "--thinking",
        "--export",
        "--extension",
        "-e",
        "--skill",
        "--prompt-template",
        "--theme",
    ]
    .into_iter()
    .collect()
});

const TAKES_NON_OPTION_VALUE: [&str; 4] = ["--mode", "--use-theme", "--tui-mode", "--list-models"];
const TAKES_NON_LONG_OPTION_VALUE: [&str; 2] = ["--session-runtime", "--listen"];

fn value_length(arg: &str, next: Option<&str>) -> usize {
    let Some(next) = next else {
        return 0;
    };
    if ALWAYS_TAKES_VALUE.contains(arg) {
        return 1;
    }
    if TAKES_NON_OPTION_VALUE.contains(&arg) {
        return usize::from(!(next.starts_with('-') || next.starts_with('@')));
    }
    if TAKES_NON_LONG_OPTION_VALUE.contains(&arg) {
        return usize::from(!next.starts_with("--"));
    }
    if arg == "--print" || arg == "-p" {
        // `---` is a message, not an option: the parser only treats `--`-prefixed text as a flag.
        return usize::from(
            !(next.starts_with('@') || (next.starts_with('-') && !next.starts_with("---"))),
        );
    }
    if arg.starts_with("--") && !arg.contains('=') {
        return usize::from(!(next.starts_with('-') || next.starts_with('@')));
    }
    0
}

/// True when this launch only prints metadata, so no session can start and the config migration
/// must not run.
pub fn is_metadata_only_launch(argv: &[String]) -> bool {
    let mut metadata = false;
    let mut index = 0;
    while index < argv.len() {
        let arg = argv[index].as_str();
        if arg == "--" {
            break;
        }
        if SESSION_HOST_OPTIONS.contains(&arg) {
            return false;
        }
        if METADATA_OPTIONS.contains(&arg) {
            metadata = true;
        }
        index += value_length(arg, argv.get(index + 1).map(String::as_str)) + 1;
    }
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_plain_launch_without_metadata_options_migrates() {
        assert!(!is_metadata_only_launch(&argv(&[
            "--extension",
            "/omo/plugin"
        ])));
        assert!(!is_metadata_only_launch(&argv(&[])));
    }

    #[test]
    fn metadata_options_gate_the_migration() {
        for items in [
            vec!["--extension", "/omo/plugin", "--help"],
            vec!["--extension", "/omo/plugin", "-h"],
            vec!["--extension", "/omo/plugin", "--list-models", "gpt"],
            vec!["--extension", "/omo/plugin", "--list-tips"],
            vec!["--extension", "/omo/plugin", "config", "--help"],
            vec!["--extension", "/omo/plugin", "-p", "hi", "--help"],
        ] {
            assert!(is_metadata_only_launch(&argv(&items)), "{items:?}");
        }
    }

    #[test]
    fn everything_after_a_double_dash_is_message_text() {
        assert!(!is_metadata_only_launch(&argv(&[
            "--extension",
            "/omo/plugin",
            "-p",
            "hi",
            "--",
            "--help"
        ])));
    }

    #[test]
    fn a_value_of_an_option_that_always_takes_one_is_never_metadata() {
        assert!(!is_metadata_only_launch(&argv(&[
            "--extension",
            "/omo/plugin",
            "-p",
            "hi",
            "--name",
            "-h"
        ])));
        assert!(!is_metadata_only_launch(&argv(&[
            "--extension",
            "/omo/plugin",
            "--append-system-prompt",
            "--help"
        ])));
    }

    #[test]
    fn a_session_host_serves_sessions_whatever_else_it_was_given() {
        assert!(!is_metadata_only_launch(&argv(&[
            "--extension",
            "/omo/plugin",
            "--mode",
            "rpc",
            "--multi-session",
            "--help"
        ])));
        assert!(!is_metadata_only_launch(&argv(&[
            "--listen",
            "127.0.0.1:0",
            "--help"
        ])));
    }

    #[test]
    fn a_value_that_looks_like_an_option_is_skipped_whole() {
        assert!(is_metadata_only_launch(&argv(&[
            "--model", "--help", "--help"
        ])));
        assert!(!is_metadata_only_launch(&argv(&["--model=--help"])));
    }

    #[test]
    fn print_swallows_a_dashed_message_but_not_a_reference_or_a_triple_dash_text() {
        assert!(is_metadata_only_launch(&argv(&["-p", "--help"])));
        assert!(is_metadata_only_launch(&argv(&["-p", "@file", "--help"])));
        assert!(is_metadata_only_launch(&argv(&["-p", "---text", "--help"])));
        assert!(!is_metadata_only_launch(&argv(&["-p", "message"])));
    }
}
