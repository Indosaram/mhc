//! Port of `packages/coding-agent/src/core/kill-guard/shim-script.ts` (senpi#5349, DESIGN-12096 §4' L2).
//!
//! The POSIX sh text of the kill guard's runtime layer: `kill`, `pkill` and `killall` shims that
//! judge their real argv before the real binary runs, `bash_env.sh` that wraps bash's builtin
//! `kill` in non-interactive bash, and `bash_kill.sh` that the PROMPT_COMMAND hook sources into
//! interactive bash.

use super::kill::refuse_all;
use super::types::{KillGuardRule, KillGuardVerdict};
use super::verdict::{KILL_GUARD_TAG, refuse};

pub const KILL_GUARD_SHIM_NAMES: [&str; 3] = ["kill", "pkill", "killall"];
pub const KILL_GUARD_BASH_ENV: &str = "bash_env.sh";
pub const KILL_GUARD_BASH_KILL: &str = "bash_kill.sh";
/// Every PROMPT_COMMAND hook starts with this, so an inherited hook is recognised as the kill guard's own.
pub const KILL_GUARD_PROMPT_HOOK_PREFIX: &str = "__senpi_kg_q=$?;";
/// A file every shim directory holds, so a shim looking for the real binary skips every kill-guard directory.
pub const KILL_GUARD_SHIM_MARKER: &str = ".senpi-kill-guard";
/// The table read and the dry run are bounded at 2 s each; the rest covers the judge's own start.
pub const KILL_GUARD_JUDGE_DEADLINE_SECONDS: u64 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillGuardShimFiles {
    pub kill: String,
    pub pkill: String,
    pub killall: String,
    pub bash_env: String,
    pub bash_kill: String,
}

impl KillGuardShimFiles {
    pub fn entries(&self) -> [(&'static str, &str); 5] {
        [
            ("kill", &self.kill),
            ("pkill", &self.pkill),
            ("killall", &self.killall),
            (KILL_GUARD_BASH_ENV, &self.bash_env),
            (KILL_GUARD_BASH_KILL, &self.bash_kill),
        ]
    }
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn refusal(verdict: KillGuardVerdict) -> String {
    match verdict {
        KillGuardVerdict::Refuse { message, .. } => message,
        _ => String::new(),
    }
}

fn library(deadline_seconds: u64) -> String {
    let all_processes = quote(&refusal(refuse_all()));
    let protected_without_judge = quote(&refusal(refuse(
        KillGuardRule::R2,
        "it names a pid the host marked protected, and the kill guard's judge is not installed to check it",
        None,
    )));
    let judge_failed = quote(&refusal(refuse(
        KillGuardRule::R5,
        "the kill guard's judge failed or did not answer in time, so nothing was signalled",
        None,
    )));
    let judge_missing = quote(&format!(
        "{KILL_GUARD_TAG} the kill guard's judge is not installed (SENPI_KILL_GUARD_EXEC), so this runs unjudged:"
    ));

    format!(
        r#"__senpi_kg_all={all_processes}
__senpi_kg_protected={protected_without_judge}
__senpi_kg_failed={judge_failed}
__senpi_kg_missing={judge_missing}
__senpi_kg_deadline={deadline_seconds}

# kill's arguments, decided in sh with no runtime start: 0 = only literal positive pids and none protected,
# 1 = the judge decides, 2 = a literal -1 target, 3 = a literal pid the host marked protected.
__senpi_kg_fast() {{
	__senpi_kg_p=0
	__senpi_kg_n=0
	if [ "$#" -eq 1 ] && [ "$1" = "-1" ]; then return 2; fi
	case ${{1-}} in
	-l | -L) return 0 ;;
	--) ;;
	-*) shift ;;
	esac
	if [ "${{1-}}" = "--" ]; then shift; fi
	if [ "$#" -eq 0 ]; then return 1; fi
	for __senpi_kg_a do
		case $__senpi_kg_a in
		-1) return 2 ;;
		'' | 0* | *[!0-9]*) __senpi_kg_n=1 ;;
		*)
			case ,${{SENPI_KILL_GUARD_PROTECTED-}}, in
			*,"$__senpi_kg_a",*) __senpi_kg_p=1 ;;
			esac
			;;
		esac
	done
	if [ "$__senpi_kg_p" -eq 1 ]; then return 3; fi
	return "$__senpi_kg_n"
}}

__senpi_kg_present() {{
	if [ -z "${{SENPI_KILL_GUARD_EXEC-}}" ] || [ ! -f "$SENPI_KILL_GUARD_EXEC" ] || [ ! -x "$SENPI_KILL_GUARD_EXEC" ]; then
		return 1
	fi
	[ -z "${{SENPI_KILL_GUARD_EXEC_ENTRY-}}" ] || [ -f "$SENPI_KILL_GUARD_EXEC_ENTRY" ]
}}

# The judge on the real argv, killed by a watchdog after the deadline: 0 allows, 126 refuses, anything else failed.
__senpi_kg_judge() (
	unset BUN_BE_BUN
	ELECTRON_RUN_AS_NODE=1
	export ELECTRON_RUN_AS_NODE
	__senpi_kg_v=$1
	shift
	if [ -n "${{SENPI_KILL_GUARD_EXEC_ENTRY-}}" ]; then
		"$SENPI_KILL_GUARD_EXEC" "$SENPI_KILL_GUARD_EXEC_ENTRY" __kill-guard check --vantage "$__senpi_kg_v" -- "$@" </dev/null &
	else
		"$SENPI_KILL_GUARD_EXEC" __kill-guard check --vantage "$__senpi_kg_v" -- "$@" </dev/null &
	fi
	__senpi_kg_j=$!
	(
		trap 'command kill "${{__senpi_kg_t-}}" 2>/dev/null; exit 0' TERM
		command sleep "$__senpi_kg_deadline" &
		__senpi_kg_t=$!
		if wait "$__senpi_kg_t"; then command kill -KILL "$__senpi_kg_j" 2>/dev/null; fi
	) </dev/null >/dev/null 2>&1 &
	__senpi_kg_w=$!
	__senpi_kg_s=0
	wait "$__senpi_kg_j" || __senpi_kg_s=$?
	command kill "$__senpi_kg_w" 2>/dev/null || :
	exit "$__senpi_kg_s"
)

# One invocation: $1 is the pid that will send the signal, the rest its argv. Returns 0 to run it, else its status.
__senpi_kg_decide() {{
	__senpi_kg_v=$1
	shift
	__senpi_kg_f=1
	if [ "$1" = kill ]; then
		shift
		__senpi_kg_f=0
		__senpi_kg_fast "$@" || __senpi_kg_f=$?
		set -- kill "$@"
		case $__senpi_kg_f in
		0) return 0 ;;
		2)
			printf '%s\n' "$__senpi_kg_all" >&2
			return 126
			;;
		esac
	fi
	if ! __senpi_kg_present; then
		if [ "$__senpi_kg_f" -eq 3 ]; then
			printf '%s\n' "$__senpi_kg_protected" >&2
			return 126
		fi
		printf '%s %s\n' "$__senpi_kg_missing" "$1" >&2
		return 0
	fi
	__senpi_kg_s=0
	__senpi_kg_judge "$__senpi_kg_v" "$@" || __senpi_kg_s=$?
	case $__senpi_kg_s in
	0) return 0 ;;
	126) return 126 ;;
	esac
	printf '%s (judge status %s)\n' "$__senpi_kg_failed" "$__senpi_kg_s" >&2
	return 126
}}
"#
    )
}

fn shim(dir: &str, name: &str, deadline_seconds: u64) -> String {
    let q_dir = quote(dir);
    let lib = library(deadline_seconds);
    format!(
        r#"#!/bin/sh
# senpi kill guard (senpi#5349): runs `{name}` only after the judge allows its real argv. Written by senpi.
__senpi_kg_dir={q_dir}
__senpi_kg_name={name}
{lib}# The real binary: the first PATH match outside every kill-guard shim directory that no outer shim already ran. A
# symlink to a shim elsewhere on PATH re-enters a shim, so each shim lists what it execs in SENPI_KILL_GUARD_VISITED and
# skips every listed path: each re-entry removes one candidate, so the walk ends.
__senpi_kg_find() (
	set -f
	IFS=:
	for __senpi_kg_d in ${{PATH-}}; do
		if [ -z "$__senpi_kg_d" ]; then __senpi_kg_d=.; fi
		if [ "$__senpi_kg_d" = "$__senpi_kg_dir" ] || [ -f "$__senpi_kg_d/{KILL_GUARD_SHIM_MARKER}" ]; then continue; fi
		case :${{SENPI_KILL_GUARD_VISITED-}}: in
		*:"$__senpi_kg_d/$1":*) continue ;;
		esac
		if [ -f "$__senpi_kg_d/$1" ] && [ -x "$__senpi_kg_d/$1" ]; then
			printf '%s\n' "$__senpi_kg_d/$1"
			exit 0
		fi
	done
	exit 1
)

__senpi_kg_s=0
__senpi_kg_decide "$$" "$__senpi_kg_name" "$@" || __senpi_kg_s=$?
if [ "$__senpi_kg_s" -ne 0 ]; then exit "$__senpi_kg_s"; fi
if __senpi_kg_real=$(__senpi_kg_find "$__senpi_kg_name"); then
	SENPI_KILL_GUARD_VISITED=${{SENPI_KILL_GUARD_VISITED:+$SENPI_KILL_GUARD_VISITED:}}$__senpi_kg_real
	export SENPI_KILL_GUARD_VISITED
	exec "$__senpi_kg_real" "$@"
fi
printf '%s: command not found\n' "$__senpi_kg_name" >&2
exit 127
"#
    )
}

fn bash_env(deadline_seconds: u64) -> String {
    let fn_def = kill_function(deadline_seconds);
    format!(
        r#"# senpi kill guard (senpi#5349): BASH_ENV for agent shells. Sources the user's own BASH_ENV first, then judges
# bash's builtin `kill` on its real argv. Written by senpi.
if [ -n "${{SENPI_KILL_GUARD_USER_BASH_ENV-}}" ] && [ "$SENPI_KILL_GUARD_USER_BASH_ENV" != "${{BASH_SOURCE[0]-}}" ] &&
	[ -f "$SENPI_KILL_GUARD_USER_BASH_ENV" ]; then
	. "$SENPI_KILL_GUARD_USER_BASH_ENV"
fi
{fn_def}"#
    )
}

fn kill_function(deadline_seconds: u64) -> String {
    let lib = library(deadline_seconds);
    format!(
        r#"{lib}
function kill {{
	local __senpi_kg_v __senpi_kg_f __senpi_kg_s __senpi_kg_p __senpi_kg_n __senpi_kg_a
	__senpi_kg_decide "${{BASHPID:-$$}}" kill "$@" || return
	command kill "$@"
}}
"#
    )
}

fn bash_kill(deadline_seconds: u64) -> String {
    let fn_def = kill_function(deadline_seconds);
    format!(
        r#"# senpi kill guard (senpi#5349): sourced into interactive bash by the PROMPT_COMMAND hook before the first prompt,
# after the user's startup files; judges bash's builtin `kill` on its real argv. Written by senpi.
{fn_def}"#
    )
}

/// The PROMPT_COMMAND that carries the `kill` wrapper into interactive bash, which reads no BASH_ENV.
pub fn kill_guard_prompt_command(dir: &str) -> String {
    let file = quote(&format!("{dir}/{KILL_GUARD_BASH_KILL}"));
    let user = "\"$SENPI_KILL_GUARD_USER_PROMPT_COMMAND\"";
    [
        KILL_GUARD_PROMPT_HOOK_PREFIX.to_owned(),
        format!(
            "if [ \"${{__senpi_kg_shell-}}\" != \"$$\" ]; then __senpi_kg_shell=$$; if [ -r {file} ]; then . {file}; fi; fi;"
        ),
        "if [ -n \"${SENPI_KILL_GUARD_USER_PROMPT_COMMAND-}\" ] && [ \"${__senpi_kg_in-}\" != \"$$\" ]; then".to_owned(),
        "__senpi_kg_rc() { return \"$1\"; }; __senpi_kg_run() { local __senpi_kg_in=$$;".to_owned(),
        format!(
            "if __senpi_kg_rc \"$__senpi_kg_q\"; then eval {user}; else eval {user}; fi; }}; __senpi_kg_run ${{1+\"$@\"}}; fi"
        ),
    ]
    .join(" ")
}

/// Every file of one shim directory; `dir` is baked into the shims so a shim never resolves to itself.
pub fn render_kill_guard_shims(dir: &str, deadline_seconds: Option<u64>) -> KillGuardShimFiles {
    let deadline = deadline_seconds.unwrap_or(KILL_GUARD_JUDGE_DEADLINE_SECONDS);
    KillGuardShimFiles {
        kill: shim(dir, "kill", deadline),
        pkill: shim(dir, "pkill", deadline),
        killall: shim(dir, "killall", deadline),
        bash_env: bash_env(deadline),
        bash_kill: bash_kill(deadline),
    }
}
