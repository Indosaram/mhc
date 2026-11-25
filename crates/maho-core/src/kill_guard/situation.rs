//! Port of `packages/coding-agent/src/core/kill-guard/situation.ts` (senpi#5349).

use super::types::{KillGuardContext, KillGuardVerdict};
use super::verdict::refuse;
use crate::bash_command_segments::{BashCommandSegment, BashCommandWord, BashCommandWrapper};

fn is_shell_name(name: &str) -> bool {
    matches!(
        name,
        "bash"
            | "sh"
            | "zsh"
            | "dash"
            | "ksh"
            | "mksh"
            | "ash"
            | "yash"
            | "posh"
            | "fish"
            | "csh"
            | "tcsh"
    )
}

/// Wrappers that keep `kill` a shell builtin; every other wrapper execs the external `kill`.
fn is_builtin_keeping(name: &str) -> bool {
    matches!(name, "command" | "builtin" | "time" | "eval" | "trap")
}

fn words_text(words: &[BashCommandWord]) -> impl Iterator<Item = &str> {
    words.iter().map(|word| word.text.as_str())
}

/// A wrapper after which the command runs with a PATH the runtime layer's shims are not on (DESIGN-12096 R5).
fn resets_path(wrapper: &BashCommandWrapper) -> bool {
    match wrapper.name.as_str() {
        "sudo" | "doas" | "chroot" | "systemd-run" => true,
        "env" => words_text(&wrapper.words).any(|text| {
            text == "-i"
                || text == "--ignore-environment"
                || text == "-"
                || text.starts_with("PATH=")
        }),
        "su" => {
            words_text(&wrapper.words).any(|text| text == "-" || text == "-l" || text == "--login")
        }
        "nix-shell" => words_text(&wrapper.words).any(|text| text == "--pure"),
        _ => false,
    }
}

/// Where one kill-family invocation runs, which decides whether a later layer can still see its real argv.
#[derive(Clone, Debug)]
pub struct Situation<'a> {
    pub segment: &'a BashCommandSegment,
    pub reset_path: bool,
    /// `kill` runs as a shell builtin.
    pub builtin: bool,
    /// The runtime layer will see this invocation's real argv (PATH shims, bash's BASH_ENV wrapper).
    pub runtime_covers: bool,
}

pub fn situation_of<'a>(
    segment: &'a BashCommandSegment,
    context: &KillGuardContext,
) -> Situation<'a> {
    let reset_path = segment.wrappers.iter().any(resets_path);
    let mut shell = context.shell.as_str();
    let mut after_shell: &[BashCommandWrapper] = &segment.wrappers;
    for index in (0..segment.wrappers.len()).rev() {
        let wrapper = &segment.wrappers[index];
        if is_shell_name(&wrapper.name) {
            shell = wrapper.name.as_str();
            after_shell = &segment.wrappers[index + 1..];
            break;
        }
    }
    let shell_runs = shell != "bun" && shell != "argv";
    let builtin = segment.name == "kill"
        && !segment.name_is_path
        && shell_runs
        && after_shell
            .iter()
            .all(|wrapper| is_builtin_keeping(&wrapper.name));
    let runtime_covers = !reset_path && !segment.name_is_path && (!builtin || shell == "bash");
    Situation {
        segment,
        reset_path,
        builtin,
        runtime_covers,
    }
}

/// The verdict for a dynamic word: defer where the runtime layer will see it, refuse UNKNOWN where nothing will.
pub fn dynamic_verdict(situation: &Situation<'_>, what: &str) -> KillGuardVerdict {
    if situation.runtime_covers {
        return super::verdict::defer(format!("{what} is only known when the command runs"));
    }
    let why = if situation.reset_path {
        "a wrapper resets PATH"
    } else if situation.segment.name_is_path {
        "the command is named by path"
    } else {
        "a shell builtin outside bash"
    };
    refuse(
        super::types::KillGuardRule::R5,
        &format!(
            "`{}` gets {what} that is only known when it runs, and nothing judges it then ({why})",
            situation.segment.name_spelling
        ),
        Some("Write the value out literally."),
    )
}
