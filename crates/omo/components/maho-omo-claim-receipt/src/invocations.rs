//! Port of `claim-receipt/invocations.ts`: which checks a shell command line actually invokes.

use std::sync::LazyLock;

use regex::Regex;

use crate::shell_words::shell_segments;

pub const MAX_DEPTH: usize = 3;

/// The kinds of work a shell command line can invoke, including read-only lookups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InvokedKind {
    Test,
    Typecheck,
    Lint,
    Build,
    Run,
    Lookup,
}

impl InvokedKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Typecheck => "typecheck",
            Self::Lint => "lint",
            Self::Build => "build",
            Self::Run => "run",
            Self::Lookup => "lookup",
        }
    }
}

static ASSIGNMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").expect("valid assignment regex"));
static SLOT_WRAPPERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|/)(?:heavy-slot|early-static-slot|launch-detached)\.sh$")
        .expect("valid slot wrapper regex")
});
static PYTHONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^python(?:3(?:\.\d+)?)?$").expect("valid python regex"));
static QA_SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|/)scripts/qa/").expect("valid qa script regex"));
static CI_API_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"check-runs|check-suites|/status(?:es)?\b").expect("valid ci api path regex")
});
static SHELL_C_FLAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-[a-z]*c$").expect("valid shell -c regex"));
static SCRIPT_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^test(?::|$)").expect("valid test script regex"));
static SCRIPT_TYPECHECK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:typecheck|type-check)(?::|$)").expect("valid typecheck script regex")
});
static SCRIPT_LINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^lint(?::|$)").expect("valid lint script regex"));
static SCRIPT_BUILD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^build(?::|$)").expect("valid build script regex"));

const SHELLS: [&str; 3] = ["bash", "sh", "zsh"];
const SHELL_KEYWORDS: [&str; 10] = [
    "do", "then", "else", "elif", "if", "while", "until", "!", "{", "}",
];
const PASSTHROUGH: [&str; 5] = ["env", "time", "nohup", "command", "sudo"];
const PYTHON_RUNNERS: [&str; 6] = ["uv", "poetry", "pdm", "pipx", "hatch", "rye"];
const TASK_RUNNERS: [&str; 3] = ["make", "just", "task"];
const RUNNERS: [&str; 4] = ["bun", "npm", "pnpm", "yarn"];
const PACKAGE_EXEC: [&str; 3] = ["bunx", "npx", "pnpx"];
const LOOKUP_TOOLS: [&str; 11] = [
    "rg", "grep", "egrep", "cat", "head", "tail", "ls", "find", "fd", "sg", "ast-grep",
];
const SSH_VALUE_FLAGS: &str = "BbcDEeFIiJLlmOoPpQRSWw";

fn contains(values: &[&str], value: &str) -> bool {
    values.contains(&value)
}

/// Wrapper options that take a value (`env -u NAME`, `sudo -u user`, `time -o file`), per env(1),
/// sudo(8) and time(1).
fn wrapper_value_flags(exe: &str) -> Option<&'static [&'static str]> {
    match exe {
        "env" => Some(&["-u", "-C", "-P"]),
        "sudo" => Some(&[
            "-u", "-g", "-h", "-p", "-c", "-C", "-D", "-r", "-R", "-t", "-U", "-T",
        ]),
        "time" => Some(&["-f", "-o"]),
        _ => None,
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn skip_flags<'a>(
    argv: &'a [String],
    from: usize,
    value_flags: Option<&[&str]>,
) -> &'a [String] {
    let mut index = from;
    while index < argv.len() && argv[index].starts_with('-') {
        let takes_value = value_flags.is_some_and(|flags| contains(flags, argv[index].as_str()));
        index += if takes_value { 2 } else { 1 };
    }
    argv.get(index..).unwrap_or(&[])
}

fn skip_timeout(argv: &[String]) -> &[String] {
    let mut index = 1;
    while index < argv.len() && argv[index].starts_with('-') {
        let flag = argv[index].as_str();
        index += if matches!(flag, "-k" | "-s" | "--kill-after" | "--signal") {
            2
        } else {
            1
        };
    }
    argv.get(index + 1..).unwrap_or(&[])
}

/// The remote command of `ssh [options] host command...`: ssh joins the remaining words with spaces.
fn ssh_remote_command(argv: &[String]) -> Option<String> {
    let mut index = 1;
    while index < argv.len() && argv[index].starts_with('-') {
        let flag = argv[index].as_str();
        let characters: Vec<char> = flag.chars().collect();
        let takes_value = characters.len() == 2
            && SSH_VALUE_FLAGS.contains(characters.get(1).copied().unwrap_or(' '));
        index += if takes_value { 2 } else { 1 };
    }
    let start = if argv.get(index + 1).map(String::as_str) == Some("--") {
        index + 2
    } else {
        index + 1
    };
    let remote = argv.get(start..).unwrap_or(&[]);
    if remote.is_empty() {
        None
    } else {
        Some(remote.join(" "))
    }
}

fn script_kind(script: &str) -> Option<InvokedKind> {
    if SCRIPT_TEST.is_match(script) {
        return Some(InvokedKind::Test);
    }
    if SCRIPT_TYPECHECK.is_match(script) {
        return Some(InvokedKind::Typecheck);
    }
    if SCRIPT_LINT.is_match(script) {
        return Some(InvokedKind::Lint);
    }
    if SCRIPT_BUILD.is_match(script) {
        return Some(InvokedKind::Build);
    }
    None
}

fn script_kinds<'a>(scripts: impl IntoIterator<Item = &'a str>) -> Vec<InvokedKind> {
    scripts.into_iter().filter_map(script_kind).collect()
}

/// GitHub CI readouts (`gh pr checks`, run views, check-run APIs) are test receipts for claims
/// about CI.
fn github_kinds(rest: &[String]) -> Vec<InvokedKind> {
    let first = rest.first().map(String::as_str).unwrap_or("");
    let second = rest.get(1).map(String::as_str).unwrap_or("");
    if first == "pr" && second == "checks" {
        return vec![InvokedKind::Test];
    }
    if first == "pr"
        && second == "view"
        && rest
            .iter()
            .any(|argument| argument.contains("statusCheckRollup"))
    {
        return vec![InvokedKind::Test];
    }
    if first == "run" && (second == "view" || second == "watch") {
        return vec![InvokedKind::Test];
    }
    if first == "api" && rest.iter().any(|argument| CI_API_PATH.is_match(argument)) {
        return vec![InvokedKind::Test];
    }
    Vec::new()
}

fn tool_kinds(exe: &str, rest: &[String]) -> Vec<InvokedKind> {
    let first = rest.first().map(String::as_str).unwrap_or("");
    let second = rest.get(1).map(String::as_str).unwrap_or("");
    if matches!(exe, "bun" | "node" | "tsx") && rest.iter().any(|argument| QA_SCRIPT.is_match(argument))
    {
        return vec![InvokedKind::Run];
    }
    if contains(&TASK_RUNNERS, exe) {
        return script_kinds(skip_flags(rest, 0, None).iter().map(String::as_str))
            .into_iter()
            .take(1)
            .collect();
    }
    if matches!(exe, "vitest" | "jest" | "pytest") {
        return vec![InvokedKind::Test];
    }
    if exe == "playwright" && first == "test" {
        return vec![InvokedKind::Test];
    }
    if matches!(exe, "tsc" | "tsgo" | "mypy" | "pyright") {
        return vec![InvokedKind::Typecheck];
    }
    if matches!(exe, "eslint" | "oxlint" | "ruff") {
        return vec![InvokedKind::Lint];
    }
    if exe == "biome" && matches!(first, "check" | "lint" | "ci") {
        return vec![InvokedKind::Lint];
    }
    if exe == "cargo" {
        match first {
            "test" => return vec![InvokedKind::Test],
            "check" => return vec![InvokedKind::Typecheck],
            "clippy" => return vec![InvokedKind::Lint],
            "build" => return vec![InvokedKind::Build],
            _ => {}
        }
    }
    if exe == "go" && (first == "test" || first == "build") {
        return vec![if first == "test" {
            InvokedKind::Test
        } else {
            InvokedKind::Build
        }];
    }
    if exe == "deno" && first == "test" {
        return vec![InvokedKind::Test];
    }
    if matches!(exe, "vite" | "next") && first == "build" {
        return vec![InvokedKind::Build];
    }
    if exe == "gh" {
        return github_kinds(rest);
    }
    if exe == "vp" {
        match first {
            "test" => return vec![InvokedKind::Test],
            "check" => return vec![InvokedKind::Typecheck, InvokedKind::Lint],
            "lint" => return vec![InvokedKind::Lint],
            "run" => return script_kinds(rest.get(1..).unwrap_or(&[]).iter().map(String::as_str)),
            _ => {}
        }
    }
    if contains(&RUNNERS, exe) {
        let kinds = if exe == "bun" && first == "test" {
            vec![InvokedKind::Test]
        } else if exe == "bun" && first == "build" {
            vec![InvokedKind::Build]
        } else if first == "run" {
            script_kinds(std::iter::once(second))
        } else if exe != "bun" && first == "test" {
            vec![InvokedKind::Test]
        } else {
            Vec::new()
        };
        if !kinds.is_empty() {
            return kinds;
        }
    }
    if contains(&LOOKUP_TOOLS, exe) {
        return vec![InvokedKind::Lookup];
    }
    if exe == "git" && (first == "grep" || first == "show") {
        return vec![InvokedKind::Lookup];
    }
    Vec::new()
}

fn wrapped_kinds(exe: &str, rest: &[String], depth: usize) -> Option<Vec<InvokedKind>> {
    if exe == "env" && rest.iter().any(|argument| argument == "-S") {
        let split = rest.iter().position(|argument| argument == "-S").unwrap_or(0);
        return Some(if depth >= MAX_DEPTH {
            Vec::new()
        } else {
            invoked_kinds(&rest.get(split + 1..).unwrap_or(&[]).join(" "), depth + 1)
        });
    }
    if contains(&PASSTHROUGH, exe) {
        return Some(argv_kinds(skip_flags(rest, 1, wrapper_value_flags(exe)), depth));
    }
    if contains(&PYTHON_RUNNERS, exe) && rest.get(1).map(String::as_str) == Some("run") {
        return Some(argv_kinds(skip_flags(rest, 2, None), depth));
    }
    if PYTHONS.is_match(exe) {
        if let Some(module) = rest.iter().position(|argument| argument == "-m") {
            return Some(argv_kinds(rest.get(module + 1..).unwrap_or(&[]), depth));
        }
        return Some(if rest.iter().any(|argument| QA_SCRIPT.is_match(argument)) {
            vec![InvokedKind::Run]
        } else {
            Vec::new()
        });
    }
    if exe == "timeout" {
        return Some(argv_kinds(skip_timeout(rest), depth));
    }
    if exe == "nice" {
        let tail = if rest.get(1).map(String::as_str) == Some("-n") {
            rest.get(3..).unwrap_or(&[])
        } else {
            rest.get(1..).unwrap_or(&[])
        };
        return Some(argv_kinds(tail, depth));
    }
    if rest
        .first()
        .is_some_and(|argument| SLOT_WRAPPERS.is_match(argument))
    {
        let separator = rest.iter().position(|argument| argument == "--");
        return Some(match separator {
            Some(separator) => argv_kinds(rest.get(separator + 1..).unwrap_or(&[]), depth),
            None => Vec::new(),
        });
    }
    if exe == "ssh" {
        let remote = ssh_remote_command(rest);
        return Some(match remote {
            Some(remote) if depth < MAX_DEPTH => invoked_kinds(&remote, depth + 1),
            _ => Vec::new(),
        });
    }
    if contains(&SHELLS, exe) {
        let flag_index = rest
            .iter()
            .enumerate()
            .position(|(index, argument)| index > 0 && SHELL_C_FLAG.is_match(argument));
        let Some(flag_index) = flag_index else {
            return Some(
                if skip_flags(rest, 1, None)
                    .first()
                    .is_some_and(|argument| QA_SCRIPT.is_match(argument))
                {
                    vec![InvokedKind::Run]
                } else {
                    Vec::new()
                },
            );
        };
        return Some(match rest.get(flag_index + 1) {
            Some(script) if depth < MAX_DEPTH => invoked_kinds(script, depth + 1),
            _ => Vec::new(),
        });
    }
    if contains(&PACKAGE_EXEC, exe) {
        let args: Vec<String> = rest
            .get(1..)
            .unwrap_or(&[])
            .iter()
            .filter(|argument| !argument.starts_with('-'))
            .cloned()
            .collect();
        return Some(argv_kinds(&args, depth));
    }
    if exe == "bun" && rest.get(1).map(String::as_str) == Some("x") {
        return Some(argv_kinds(rest.get(2..).unwrap_or(&[]), depth));
    }
    if exe == "pnpm" && rest.get(1).map(String::as_str) == Some("exec") {
        return Some(argv_kinds(rest.get(2..).unwrap_or(&[]), depth));
    }
    None
}

fn argv_kinds(argv: &[String], depth: usize) -> Vec<InvokedKind> {
    let mut rest = argv;
    while let Some(head) = rest.first() {
        if ASSIGNMENT.is_match(head) || contains(&SHELL_KEYWORDS, head.as_str()) {
            rest = rest.get(1..).unwrap_or(&[]);
        } else {
            break;
        }
    }
    let Some(head) = rest.first() else {
        return Vec::new();
    };
    if QA_SCRIPT.is_match(head) {
        return vec![InvokedKind::Run];
    }
    let exe = basename(head);
    wrapped_kinds(exe, rest, depth).unwrap_or_else(|| tool_kinds(exe, rest.get(1..).unwrap_or(&[])))
}

/// Kinds of checks a shell command line actually invokes: the executable and its subcommand
/// decide, never a mention.
#[must_use]
pub fn invoked_kinds(line: &str, depth: usize) -> Vec<InvokedKind> {
    let mut kinds: Vec<InvokedKind> = Vec::new();
    for argv in shell_segments(line) {
        for kind in argv_kinds(&argv, depth) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<&'static str> {
        invoked_kinds(line, 0)
            .into_iter()
            .map(InvokedKind::as_str)
            .collect()
    }

    #[test]
    fn the_executable_and_its_subcommand_decide_not_a_mention() {
        assert_eq!(kinds("echo bun test"), Vec::<&'static str>::new());
        assert_eq!(kinds("cargo test"), vec!["test"]);
        assert_eq!(kinds("vp check"), vec!["typecheck", "lint"]);
        assert_eq!(kinds("gh pr checks 12"), vec!["test"]);
        assert_eq!(kinds("gh pr view 9 --json state"), Vec::<&'static str>::new());
    }

    #[test]
    fn wrappers_and_remote_commands_still_reach_the_real_check() {
        assert_eq!(kinds("env -u FOO bun test x"), vec!["test"]);
        assert_eq!(kinds("sudo -u bob bun test x"), vec!["test"]);
        assert_eq!(kinds("time -p bun test x"), vec!["test"]);
        assert_eq!(kinds("uv run pytest tests/"), vec!["test"]);
        assert_eq!(kinds("python3 -m mypy src"), vec!["typecheck"]);
        assert_eq!(
            kinds("ssh -o BatchMode=yes -p 22 host 'cd w && bun test x'"),
            vec!["test"]
        );
        assert_eq!(kinds("bash -c 'bunx tsgo --noEmit -p x'"), vec!["typecheck"]);
    }
}
