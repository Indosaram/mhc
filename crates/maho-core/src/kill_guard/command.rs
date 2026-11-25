//! Port of `packages/coding-agent/src/core/kill-guard/command.ts` (senpi#5349).

use super::kill::{KillTarget, judge_kill, judge_targets, refuse_all};
use super::options::SelectingTool;
use super::selecting::judge_selecting;
use super::situation::situation_of;
use super::types::{
    KillGuardContext, KillGuardRule, KillGuardShell, KillGuardVerdict, SignalCall, SignalId,
};
use super::verdict::{ALLOW, combine, defer, refuse};
use crate::bash_command_segments::{
    BashCommandSegment, BashCommandWordsAnalysis, analyze_bash_command_words,
    best_effort_command_lines,
};

fn is_kill_family(name: &str) -> bool {
    matches!(name, "kill" | "pkill" | "killall")
}

fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn piped_into_kill(segment_index: usize, segments: &[BashCommandSegment]) -> bool {
    let pipeline = segments[segment_index].pipeline;
    segments
        .iter()
        .skip(segment_index + 1)
        .any(|other| other.pipeline == pipeline && is_kill_family(&other.name))
}

/// The command line nix-shell runs (`--run`/`--command`), or the applet line busybox runs (`busybox sh -c ...`).
fn inner_script(segment: &BashCommandSegment) -> Option<String> {
    if segment.name == "busybox" {
        return if segment.words.iter().any(|word| word.dynamic) {
            None
        } else {
            Some(
                segment
                    .words
                    .iter()
                    .map(|word| quote(&word.text))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        };
    }
    let index = segment
        .words
        .iter()
        .position(|word| word.text == "--run" || word.text == "--command")?;
    let script = segment.words.get(index + 1)?;
    (!script.dynamic).then(|| script.text.clone())
}

/// nix-shell, busybox, chroot and systemd-run run a command the lexer does not unwrap; the guard reads it from their words.
fn unwrap_opaque(segment: &BashCommandSegment) -> Vec<BashCommandSegment> {
    let wrapper = crate::bash_command_segments::BashCommandWrapper {
        name: segment.name.clone(),
        words: segment.words.clone(),
    };
    if segment.name == "nix-shell" || segment.name == "busybox" {
        let Some(script) = inner_script(segment) else {
            return Vec::new();
        };
        let BashCommandWordsAnalysis::Parsed { segments: inner } =
            analyze_bash_command_words(&script)
        else {
            return Vec::new();
        };
        return inner
            .into_iter()
            .map(|child| {
                let mut wrappers = segment.wrappers.clone();
                wrappers.push(wrapper.clone());
                wrappers.extend(child.wrappers);
                BashCommandSegment { wrappers, ..child }
            })
            .collect();
    }
    if segment.name != "chroot" && segment.name != "systemd-run" {
        return Vec::new();
    }
    let Some(index) = segment
        .words
        .iter()
        .position(|word| !word.dynamic && is_kill_family(basename(&word.text)))
    else {
        return Vec::new();
    };
    let command = &segment.words[index];
    let mut wrappers = segment.wrappers.clone();
    wrappers.push(wrapper);
    vec![BashCommandSegment {
        name: basename(&command.text).to_owned(),
        name_spelling: command.text.clone(),
        name_is_path: command.text.contains('/'),
        words: segment.words[index + 1..].to_vec(),
        wrappers,
        pipeline: segment.pipeline,
    }]
}

fn judge_segment(
    segment: &BashCommandSegment,
    segment_index: usize,
    segments: &[BashCommandSegment],
    context: &KillGuardContext,
) -> KillGuardVerdict {
    let situation = situation_of(segment, context);
    match segment.name.as_str() {
        "kill" => judge_kill(&situation, context),
        "pkill" => judge_selecting(&situation, SelectingTool::Pkill, context, false),
        "killall" => judge_selecting(&situation, SelectingTool::Killall, context, false),
        "pgrep" => judge_selecting(
            &situation,
            SelectingTool::Pgrep,
            context,
            piped_into_kill(segment_index, segments),
        ),
        _ => combine(
            unwrap_opaque(segment)
                .iter()
                .map(|inner| judge_segment(inner, segment_index, segments, context)),
        ),
    }
}

/// Replaces `/(^|[\s'"])-S/g` with `"$1"`.
fn strip_glued_env_split_string(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        let prev_matches =
            i == 0 || chars[i - 1].is_whitespace() || matches!(chars[i - 1], '\'' | '"');
        if prev_matches && chars[i] == '-' && chars.get(i + 1) == Some(&'S') {
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_short_flag_word(word: &str) -> bool {
    let Some(rest) = word.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A command the lexer cannot parse is refused only for a literal `kill -1`; other kill words are left to runtime.
fn judge_unparsed(command: &str) -> KillGuardVerdict {
    // `env -S` / `--split-string` take a whole command line inside one word (`--split-string=kill -9 -1`, `-Skill -1`):
    // the words after `=` and after a glued `-S` are read as command starts too.
    let variants = [
        command.to_owned(),
        command.replace('=', " "),
        strip_glued_env_split_string(command),
    ];
    let lines: Vec<Vec<String>> = variants
        .iter()
        .flat_map(|text| best_effort_command_lines(text))
        .collect();
    for words in &lines {
        let name = words.first().map(String::as_str);
        if name != Some("kill") {
            continue;
        }
        let second = words.get(1).map(String::as_str);
        let third = words.get(2).map(String::as_str);
        let signal_then_all =
            (second == Some("--") || second.is_some_and(is_short_flag_word)) && third == Some("-1");
        if second == Some("-1") || signal_then_all {
            return refuse_all();
        }
    }
    if lines
        .iter()
        .any(|words| words.first().is_some_and(|name| is_kill_family(name)))
    {
        return defer("the command cannot be parsed before it runs");
    }
    ALLOW
}

/// Judges every kill-family invocation a command line would run (L1, DESIGN-12096 §4').
pub fn judge_command(command: &str, context: &KillGuardContext) -> KillGuardVerdict {
    if !command.contains("kill") {
        return ALLOW;
    }
    match analyze_bash_command_words(command) {
        BashCommandWordsAnalysis::Unparsed => judge_unparsed(command),
        BashCommandWordsAnalysis::Parsed { segments } => combine(
            segments
                .iter()
                .enumerate()
                .map(|(index, segment)| judge_segment(segment, index, &segments, context)),
        ),
    }
}

/// Judges a spawn argv (no shell reads it); `["sh", "-c", script]` is read through the script.
pub fn judge_argv(argv: &[String], context: &KillGuardContext) -> KillGuardVerdict {
    if !argv.iter().any(|word| word.contains("kill")) {
        return ALLOW;
    }
    let command = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    let mut ctx = context.clone();
    ctx.shell = KillGuardShell::Argv;
    judge_command(&command, &ctx)
}

fn signal_target(call: &SignalCall) -> Option<KillTarget> {
    if call.group {
        return (call.target >= 0).then_some(KillTarget::Group(call.target));
    }
    if call.target == -1 {
        return Some(KillTarget::All);
    }
    if call.target <= 0 {
        return Some(KillTarget::Group(-call.target));
    }
    Some(KillTarget::Pid(call.target))
}

/// Judges `process.kill` / `os.kill` / `os.killpg` on a numeric target (E6, E8).
pub fn judge_signal(call: &SignalCall, context: &KillGuardContext) -> KillGuardVerdict {
    let is_zero = match &call.signal {
        Some(SignalId::Number(0)) => true,
        Some(SignalId::Name(name)) => name == "0" || name == "SIG0",
        _ => false,
    };
    if is_zero {
        return ALLOW;
    }
    let Some(target) = signal_target(call) else {
        return refuse(
            KillGuardRule::R5,
            &format!(
                "a negative process group {} cannot be signalled",
                call.target
            ),
            None,
        );
    };
    if target == KillTarget::All {
        return refuse_all();
    }
    if context.platform == "win32" {
        return ALLOW;
    }
    judge_targets(&[target], context)
}
