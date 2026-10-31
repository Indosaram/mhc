//! Port of `permission-system/auto-program-rules.ts` (senpi 903b8336): the read-only programs the
//! `auto` preset may run, with the grammar each one's words are classified by.

use crate::{
    auto_shell_grammar::{
        ClassifiedWord, FlagSpec, OperandRole, ProgramSpec, WordRole, classify_words,
    },
    auto_shell_segments::ShellWord,
};
use regex::Regex;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

/// Classifies a command's words, or `None` when the program or any word is not understood.
pub type ProgramRule = Arc<dyn Fn(&[ShellWord]) -> Option<Vec<ClassifiedWord>> + Send + Sync>;

fn all(role: WordRole) -> OperandRole {
    Arc::new(move |_: usize, _: usize, _: &str| role)
}
fn first_is(first: WordRole, rest: WordRole) -> OperandRole {
    Arc::new(move |index: usize, _: usize, _: &str| if index == 0 { first } else { rest })
}

fn flags(names: &str, role: FlagSpec) -> HashMap<String, FlagSpec> {
    names
        .split(' ')
        .map(|name| (name.to_owned(), role))
        .collect()
}
fn no_value(names: &str) -> HashMap<String, FlagSpec> {
    flags(names, FlagSpec::NoValue)
}
fn valued(names: &str, role: WordRole) -> HashMap<String, FlagSpec> {
    flags(names, FlagSpec::Value(role))
}
fn merged(
    mut left: HashMap<String, FlagSpec>,
    right: HashMap<String, FlagSpec>,
) -> HashMap<String, FlagSpec> {
    left.extend(right);
    left
}

fn program(
    flags: HashMap<String, FlagSpec>,
    operand: OperandRole,
    min_operands: Option<usize>,
    max_operands: Option<usize>,
) -> ProgramSpec {
    ProgramSpec {
        flags,
        operand,
        min_operands,
        max_operands,
    }
}

fn spec(value: ProgramSpec) -> ProgramRule {
    Arc::new(move |args: &[ShellWord]| classify_words(args, &value))
}

static CLUSTER_WITH_E: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-[^-]*e").expect("static regex"));

/// grep and rg: the first operand is the pattern unless a pattern came from `-e`/`--regexp`, in
/// which case every operand is a file to read.
fn search_spec(value: ProgramSpec) -> ProgramRule {
    Arc::new(move |args: &[ShellWord]| {
        let pattern_from_flag = args.iter().any(|word| {
            word.text == "--regexp"
                || word.text.starts_with("--regexp=")
                || CLUSTER_WITH_E.is_match(&word.text)
        });
        if pattern_from_flag {
            let mut adjusted = value.clone();
            adjusted.operand = all(WordRole::ReadFile);
            adjusted.min_operands = Some(1);
            classify_words(args, &adjusted)
        } else {
            classify_words(args, &value)
        }
    })
}

/// `READ_FILES` with the given flags: every operand is a file to read, at least one.
fn read_files(flags: HashMap<String, FlagSpec>) -> ProgramSpec {
    program(flags, all(WordRole::ReadFile), Some(1), None)
}

fn specs() -> Vec<(&'static str, ProgramRule)> {
    let head_tail = || {
        read_files(merged(
            valued("-n -c --lines --bytes", WordRole::Text),
            no_value("-q -v --quiet"),
        ))
    };
    let search_value_flags = || {
        valued(
            "-e -m -A -B -C --regexp --max-count --after-context --before-context --context",
            WordRole::Text,
        )
    };
    vec![
        (
            "pwd",
            spec(program(HashMap::new(), all(WordRole::Text), None, Some(0))),
        ),
        (
            "true",
            spec(program(HashMap::new(), all(WordRole::Text), None, Some(0))),
        ),
        (
            "echo",
            spec(program(
                no_value("-n -e -E"),
                all(WordRole::Text),
                None,
                None,
            )),
        ),
        (
            "which",
            spec(program(no_value("-a"), all(WordRole::Text), Some(1), None)),
        ),
        (
            "ls",
            spec(program(
                no_value(
                    "-a -A -l -h -1 -R -t -S -r -d -F -p --all --almost-all --human-readable --recursive",
                ),
                all(WordRole::List),
                None,
                None,
            )),
        ),
        ("cat", spec(read_files(no_value("-n -b -s -A -e -t -v")))),
        ("head", spec(head_tail())),
        ("tail", spec(head_tail())),
        (
            "wc",
            spec(read_files(no_value(
                "-l -w -c -m -L --lines --words --bytes --chars",
            ))),
        ),
        (
            "diff",
            spec(program(
                merged(
                    no_value("-u -q -s -w -b -B -i --brief"),
                    valued("-U --unified", WordRole::Text),
                ),
                all(WordRole::ReadFile),
                Some(2),
                Some(2),
            )),
        ),
        (
            "stat",
            spec(program(no_value("-L"), all(WordRole::List), Some(1), None)),
        ),
        (
            "file",
            spec(program(
                no_value("-b -i -L --brief --mime"),
                all(WordRole::ReadFile),
                Some(1),
                None,
            )),
        ),
        (
            "cut",
            spec(read_files(merged(
                valued(
                    "-d -f -c -b --delimiter --fields --characters --bytes",
                    WordRole::Text,
                ),
                no_value("-s --only-delimited"),
            ))),
        ),
        (
            "grep",
            search_spec(program(
                merged(
                    no_value(
                        "-i -n -c -l -L -v -w -x -o -q -s -H -h -F -E --ignore-case --line-number --count --fixed-strings --extended-regexp --files-with-matches",
                    ),
                    search_value_flags(),
                ),
                first_is(WordRole::Text, WordRole::ReadFile),
                Some(2),
                None,
            )),
        ),
        (
            "rg",
            search_spec(program(
                merged(
                    no_value(
                        "-i -n -c -l -v -w -x -o -F -S -N --ignore-case --line-number --count --fixed-strings --smart-case --files-with-matches --no-heading",
                    ),
                    search_value_flags(),
                ),
                first_is(WordRole::Text, WordRole::ReadFile),
                Some(2),
                None,
            )),
        ),
    ]
}

static GIT_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.@-]*$").expect("static regex"));
static OBJECT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{4,64}$").expect("static regex"));

/// A git operand that is not shaped like a ref is a path; a ref-shaped one (`HEAD`, `main`) is
/// still a path when something exists at that name, which the judge checks.
fn git_operand() -> OperandRole {
    Arc::new(|_: usize, _: usize, text: &str| {
        if GIT_REF.is_match(text) && !text.contains("..") {
            WordRole::RefOrPath
        } else {
            WordRole::List
        }
    })
}

fn git_read_subcommands() -> HashMap<&'static str, ProgramSpec> {
    HashMap::from([
        (
            "status",
            program(
                no_value("-s -b -u --short --branch --porcelain --untracked-files"),
                all(WordRole::List),
                None,
                None,
            ),
        ),
        (
            "diff",
            program(
                no_value(
                    "--stat --cached --staged --name-only --name-status --numstat --no-color --color -w",
                ),
                git_operand(),
                None,
                None,
            ),
        ),
        (
            "log",
            program(
                merged(
                    no_value(
                        "--oneline --graph --decorate --stat --no-color --all --reverse --name-only --name-status",
                    ),
                    valued(
                        "-n --max-count --since --until --author --format --pretty",
                        WordRole::Text,
                    ),
                ),
                git_operand(),
                None,
                None,
            ),
        ),
        (
            "rev-parse",
            program(
                no_value("--abbrev-ref --short --show-toplevel --verify"),
                git_operand(),
                None,
                None,
            ),
        ),
        (
            "ls-files",
            program(
                no_value("-m -o -d -s --modified --others --deleted --stage --exclude-standard"),
                all(WordRole::List),
                None,
                None,
            ),
        ),
        (
            "blame",
            program(
                merged(no_value("-w -s --porcelain"), valued("-L", WordRole::Text)),
                all(WordRole::ReadFile),
                Some(1),
                None,
            ),
        ),
        (
            "branch",
            program(
                no_value("-a -r -v -vv --all --remotes --verbose --list --show-current"),
                all(WordRole::Text),
                None,
                Some(0),
            ),
        ),
    ])
}

const SUMMARY_ONLY: &[&str] = &["--stat", "--name-only", "--name-status", "--numstat"];

/// Read-only git subcommands only; any global option (`-c`, `-C`, `--git-dir`, a pager) asks.
/// `diff` passes only in a summary form; `show` is not listed; no operand may name an object path
/// or a full object id, and an operand that is not a plain ref is checked as a path.
fn git_rule() -> ProgramRule {
    let subcommands = git_read_subcommands();
    Arc::new(move |args: &[ShellWord]| {
        let (sub, rest) = args.split_first()?;
        let sub_spec = subcommands.get(sub.text.as_str())?;
        // A flag's value (`-n 1000`) is not an object id; every other word is checked.
        let is_flag_value = |index: usize| {
            index
                .checked_sub(1)
                .and_then(|previous| rest.get(previous))
                .is_some_and(|previous| {
                    matches!(sub_spec.flags.get(&previous.text), Some(FlagSpec::Value(_)))
                })
        };
        if rest.iter().enumerate().any(|(index, word)| {
            word.text.contains(':') || (!is_flag_value(index) && OBJECT_ID.is_match(&word.text))
        }) {
            return None;
        }
        if sub.text == "diff"
            && !rest
                .iter()
                .any(|word| SUMMARY_ONLY.contains(&word.text.as_str()))
        {
            return None;
        }
        classify_words(rest, sub_spec)
    })
}

pub static PROGRAM_RULES: LazyLock<HashMap<&'static str, ProgramRule>> = LazyLock::new(|| {
    let mut rules: HashMap<&'static str, ProgramRule> = specs().into_iter().collect();
    rules.insert("git", git_rule());
    rules
});
