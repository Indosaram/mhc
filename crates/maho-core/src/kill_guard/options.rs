//! Port of `packages/coding-agent/src/core/kill-guard/options.ts` (senpi#5349).

use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionAction {
    Forward,
    Drop,
    Skip,
    Allow,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallerRelativeKind {
    Pgid,
    Sid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionSpec {
    pub takes_value: bool,
    pub action: OptionAction,
    pub short: Option<&'static str>,
    pub exact: bool,
    pub regex: bool,
    pub groups: bool,
    pub ancestors: bool,
    pub ignore_ancestors: bool,
    pub signal: bool,
    pub selector: bool,
    pub caller_relative: Option<CallerRelativeKind>,
    pub ignore_case: bool,
}

impl OptionSpec {
    const fn flag(action: OptionAction) -> Self {
        Self {
            takes_value: false,
            action,
            short: None,
            exact: false,
            regex: false,
            groups: false,
            ancestors: false,
            ignore_ancestors: false,
            signal: false,
            selector: false,
            caller_relative: None,
            ignore_case: false,
        }
    }

    const fn value(action: OptionAction) -> Self {
        Self {
            takes_value: true,
            ..Self::flag(action)
        }
    }

    const fn with_exact(mut self) -> Self {
        self.exact = true;
        self
    }

    const fn with_regex(mut self) -> Self {
        self.regex = true;
        self
    }

    const fn with_groups(mut self) -> Self {
        self.groups = true;
        self
    }

    const fn with_ancestors(mut self) -> Self {
        self.ancestors = true;
        self
    }

    const fn with_ignore_ancestors(mut self) -> Self {
        self.ignore_ancestors = true;
        self
    }

    const fn with_signal(mut self) -> Self {
        self.signal = true;
        self
    }

    const fn with_selector(mut self) -> Self {
        self.selector = true;
        self
    }

    const fn with_caller_relative(mut self, kind: CallerRelativeKind) -> Self {
        self.caller_relative = Some(kind);
        self
    }

    const fn with_ignore_case(mut self) -> Self {
        self.ignore_case = true;
        self
    }
}

pub type OptionTable = HashMap<&'static str, OptionSpec>;

fn build_table(
    entries: &[(&'static str, OptionSpec)],
    aliases: &[(&'static str, &'static str)],
) -> OptionTable {
    let mut map: OptionTable = entries.iter().copied().collect();
    for &(alias, target) in aliases {
        if let Some(mut spec) = map.get(target).copied() {
            if spec.short.is_none() {
                spec.short = Some(target);
            }
            map.insert(alias, spec);
        }
    }
    map
}

/// procps-ng `pkill`/`pgrep` (Linux).
static PROCPS: LazyLock<OptionTable> = LazyLock::new(|| {
    build_table(
        &[
            ("-f", OptionSpec::flag(OptionAction::Forward)),
            ("-x", OptionSpec::flag(OptionAction::Forward).with_exact()),
            (
                "-i",
                OptionSpec::flag(OptionAction::Forward).with_ignore_case(),
            ),
            ("-n", OptionSpec::flag(OptionAction::Forward)),
            ("-o", OptionSpec::flag(OptionAction::Forward)),
            ("-v", OptionSpec::flag(OptionAction::Forward)),
            (
                "-A",
                OptionSpec::flag(OptionAction::Forward).with_ignore_ancestors(),
            ),
            ("-H", OptionSpec::flag(OptionAction::Forward)),
            ("-L", OptionSpec::flag(OptionAction::Forward)),
            (
                "-u",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-U",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-g",
                OptionSpec::value(OptionAction::Forward)
                    .with_selector()
                    .with_caller_relative(CallerRelativeKind::Pgid),
            ),
            (
                "-G",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-P",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-s",
                OptionSpec::value(OptionAction::Forward)
                    .with_selector()
                    .with_caller_relative(CallerRelativeKind::Sid),
            ),
            (
                "-t",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-r",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-F",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-O",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "--ns",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            ("--nslist", OptionSpec::value(OptionAction::Forward)),
            (
                "--cgroup",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "--env",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            ("-e", OptionSpec::flag(OptionAction::Drop)),
            ("-q", OptionSpec::value(OptionAction::Drop)),
            (
                "--signal",
                OptionSpec::value(OptionAction::Drop).with_signal(),
            ),
            ("-c", OptionSpec::flag(OptionAction::Skip)),
            ("-l", OptionSpec::flag(OptionAction::Skip)),
            ("-a", OptionSpec::flag(OptionAction::Skip)),
            ("-w", OptionSpec::flag(OptionAction::Skip)),
            ("-d", OptionSpec::value(OptionAction::Skip)),
            ("-h", OptionSpec::flag(OptionAction::Allow)),
            ("-V", OptionSpec::flag(OptionAction::Allow)),
        ],
        &[
            ("--full", "-f"),
            ("--exact", "-x"),
            ("--ignore-case", "-i"),
            ("--newest", "-n"),
            ("--oldest", "-o"),
            ("--inverse", "-v"),
            ("--ignore-ancestors", "-A"),
            ("--require-handler", "-H"),
            ("--logpidfile", "-L"),
            ("--euid", "-u"),
            ("--uid", "-U"),
            ("--pgroup", "-g"),
            ("--group", "-G"),
            ("--parent", "-P"),
            ("--session", "-s"),
            ("--terminal", "-t"),
            ("--runstates", "-r"),
            ("--pidfile", "-F"),
            ("--older", "-O"),
            ("--echo", "-e"),
            ("--queue", "-q"),
            ("--count", "-c"),
            ("--list-name", "-l"),
            ("--list-full", "-a"),
            ("--lightweight", "-w"),
            ("--delimiter", "-d"),
            ("--help", "-h"),
            ("--version", "-V"),
        ],
    )
});

/// BSD `pkill`/`pgrep` (macOS).
static BSD_PGREP: LazyLock<OptionTable> = LazyLock::new(|| {
    build_table(
        &[
            ("-f", OptionSpec::flag(OptionAction::Forward)),
            ("-x", OptionSpec::flag(OptionAction::Forward).with_exact()),
            (
                "-i",
                OptionSpec::flag(OptionAction::Forward).with_ignore_case(),
            ),
            ("-L", OptionSpec::flag(OptionAction::Forward)),
            ("-n", OptionSpec::flag(OptionAction::Forward)),
            ("-o", OptionSpec::flag(OptionAction::Forward)),
            ("-v", OptionSpec::flag(OptionAction::Forward)),
            ("-q", OptionSpec::flag(OptionAction::Forward)),
            (
                "-a",
                OptionSpec::flag(OptionAction::Forward).with_ancestors(),
            ),
            (
                "-u",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-U",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-g",
                OptionSpec::value(OptionAction::Forward)
                    .with_selector()
                    .with_caller_relative(CallerRelativeKind::Pgid),
            ),
            (
                "-G",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-P",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-s",
                OptionSpec::value(OptionAction::Forward)
                    .with_selector()
                    .with_caller_relative(CallerRelativeKind::Sid),
            ),
            (
                "-t",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-F",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            ("-I", OptionSpec::flag(OptionAction::Drop)),
            ("-l", OptionSpec::flag(OptionAction::Skip)),
            ("-d", OptionSpec::value(OptionAction::Skip)),
        ],
        &[],
    )
});

/// psmisc `killall` (Linux).
static PSMISC_KILLALL: LazyLock<OptionTable> = LazyLock::new(|| {
    build_table(
        &[
            ("-e", OptionSpec::flag(OptionAction::Forward).with_exact()),
            (
                "-I",
                OptionSpec::flag(OptionAction::Forward).with_ignore_case(),
            ),
            ("-g", OptionSpec::flag(OptionAction::Forward).with_groups()),
            ("-r", OptionSpec::flag(OptionAction::Forward).with_regex()),
            (
                "-u",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            ("-i", OptionSpec::flag(OptionAction::Drop)),
            ("-q", OptionSpec::flag(OptionAction::Drop)),
            ("-v", OptionSpec::flag(OptionAction::Drop)),
            ("-w", OptionSpec::flag(OptionAction::Drop)),
            ("-s", OptionSpec::value(OptionAction::Drop).with_signal()),
            ("-l", OptionSpec::flag(OptionAction::Allow)),
            ("-V", OptionSpec::flag(OptionAction::Allow)),
            ("-y", OptionSpec::value(OptionAction::Unknown)),
            ("-o", OptionSpec::value(OptionAction::Unknown)),
            ("-n", OptionSpec::value(OptionAction::Unknown)),
            ("-Z", OptionSpec::value(OptionAction::Unknown)),
        ],
        &[
            ("--exact", "-e"),
            ("--ignore-case", "-I"),
            ("--process-group", "-g"),
            ("--regexp", "-r"),
            ("--user", "-u"),
            ("--interactive", "-i"),
            ("--quiet", "-q"),
            ("--verbose", "-v"),
            ("--wait", "-w"),
            ("--signal", "-s"),
            ("--list", "-l"),
            ("--version", "-V"),
            ("--younger-than", "-y"),
            ("--older-than", "-o"),
            ("--ns", "-n"),
            ("--context", "-Z"),
        ],
    )
});

/// BSD `killall` (macOS). `-s` and `-d` only show what would be signalled.
static BSD_KILLALL: LazyLock<OptionTable> = LazyLock::new(|| {
    build_table(
        &[
            ("-e", OptionSpec::flag(OptionAction::Drop)),
            ("-m", OptionSpec::flag(OptionAction::Forward).with_regex()),
            (
                "-u",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-t",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            (
                "-c",
                OptionSpec::value(OptionAction::Forward).with_selector(),
            ),
            ("-v", OptionSpec::flag(OptionAction::Drop)),
            ("-q", OptionSpec::flag(OptionAction::Drop)),
            ("-z", OptionSpec::flag(OptionAction::Drop)),
            ("-I", OptionSpec::flag(OptionAction::Drop)),
            ("-s", OptionSpec::flag(OptionAction::Allow)),
            ("-d", OptionSpec::flag(OptionAction::Allow)),
            ("-l", OptionSpec::flag(OptionAction::Allow)),
            ("-help", OptionSpec::flag(OptionAction::Allow)),
        ],
        &[],
    )
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectingTool {
    Pkill,
    Pgrep,
    Killall,
}

pub fn option_table(tool: SelectingTool, platform: &str) -> &'static OptionTable {
    let linux = platform == "linux";
    match tool {
        SelectingTool::Killall => {
            if linux {
                &PSMISC_KILLALL
            } else {
                &BSD_KILLALL
            }
        }
        SelectingTool::Pkill | SelectingTool::Pgrep => {
            if linux {
                &PROCPS
            } else {
                &BSD_PGREP
            }
        }
    }
}

const SIGNAL_NAMES: &[&str] = &[
    "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "IOT", "BUS", "EMT", "FPE", "KILL", "USR1",
    "SEGV", "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CLD", "CONT", "STOP", "TSTP",
    "TTIN", "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO", "POLL", "PWR", "SYS",
    "INFO", "LOST", "UNUSED", "RTMIN", "RTMAX",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedSignal {
    pub zero: bool,
}

/// The signal a word names (`9`, `KILL`, `SIGKILL`, `rtmin+3`), or `None` when it names none.
pub fn signal_number_or_name(text: &str) -> Option<ParsedSignal> {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        let num = text.parse::<u64>().ok();
        return Some(ParsedSignal {
            zero: num == Some(0),
        });
    }
    let upper = text.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    if SIGNAL_NAMES.contains(&name) {
        return Some(ParsedSignal { zero: false });
    }
    if let Some(tail) = name
        .strip_prefix("RTMIN")
        .or_else(|| name.strip_prefix("RTMAX"))
        && let Some(digits) = tail.strip_prefix('+').or_else(|| tail.strip_prefix('-'))
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(ParsedSignal { zero: false });
    }
    if name == "ZERO" || name == "NULL" {
        return Some(ParsedSignal { zero: true });
    }
    None
}
