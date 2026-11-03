//! Port of senpi `cli/cross-project-session.ts` (pin 903b8336).
//!
//! Decides what `--session <id>` does with a session recorded under another project path (#2181).
//! A session from the same repository is offered a rebind. A different or unrecognised repository
//! keeps the fork prompt. Without an interactive session nothing is asked: the exact commands are
//! printed and the caller exits non-zero.
//!
//! senpi's `confirm` returns a promise. Here it is a blocking callback, and the caller decides how
//! to drive its dialog.

/// senpi `RepositoryMatch` (core `repository-identity.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryMatch {
    Same,
    Different,
    Unknown,
}

/// senpi `CrossProjectAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrossProjectAction {
    Rebind,
    Fork,
    Abort,
    Fail,
}

pub const FORK_PROMPT: &str = "Fork this session into current directory?";
pub const REBIND_PROMPT: &str = "Move this session here and continue it?";

const RED: (&str, &str) = ("\x1b[31m", "\x1b[39m");
const YELLOW: (&str, &str) = ("\x1b[33m", "\x1b[39m");
const DIM: (&str, &str) = ("\x1b[2m", "\x1b[22m");

fn paint((open, close): (&str, &str), text: &str) -> String {
    format!("{open}{text}{close}")
}

/// senpi `CrossProjectChoice`.
pub struct CrossProjectChoice<'a> {
    pub session_arg: &'a str,
    pub session_cwd: &'a str,
    pub cwd: &'a str,
    pub repository_match: RepositoryMatch,
    pub interactive: bool,
    pub confirm: &'a mut dyn FnMut(&str) -> bool,
    pub out: &'a mut dyn FnMut(&str),
    pub err: &'a mut dyn FnMut(&str),
}

/// senpi `chooseCrossProjectAction`.
pub fn choose_cross_project_action(choice: CrossProjectChoice<'_>) -> CrossProjectAction {
    let CrossProjectChoice {
        session_arg,
        session_cwd,
        cwd,
        repository_match,
        interactive,
        confirm,
        out,
        err,
    } = choice;
    let rebind_hint = format!(
        "If this directory is the same repository moved from {session_cwd}, use --rebind '{session_arg}' to move the session here."
    );
    if !interactive {
        err(&paint(
            RED,
            &format!("Session found in different project: {session_cwd}"),
        ));
        if repository_match == RepositoryMatch::Same {
            err(&paint(
                RED,
                &format!(
                    "{cwd} is the same git repository. Cannot confirm without an interactive session. Use --rebind '{session_arg}' to move the session here, or --fork '{session_arg}' to copy it into a new session."
                ),
            ));
            return CrossProjectAction::Fail;
        }
        err(&paint(
            RED,
            &format!(
                "Cannot confirm forking without an interactive session. Use --fork '{session_arg}' to fork it into the current directory, or re-run interactively from {session_cwd}."
            ),
        ));
        if repository_match == RepositoryMatch::Unknown {
            err(&paint(RED, &rebind_hint));
        }
        return CrossProjectAction::Fail;
    }

    out(&paint(
        YELLOW,
        &format!("Session found in different project: {session_cwd}"),
    ));
    if repository_match == RepositoryMatch::Same {
        return if confirm_same_repository_rebind(session_arg, cwd, confirm, out) {
            CrossProjectAction::Rebind
        } else {
            CrossProjectAction::Abort
        };
    }
    if repository_match == RepositoryMatch::Unknown {
        out(&paint(DIM, &rebind_hint));
    }
    if confirm(FORK_PROMPT) {
        CrossProjectAction::Fork
    } else {
        CrossProjectAction::Abort
    }
}

/// senpi `confirmSameRepositoryRebind`.
pub fn confirm_same_repository_rebind(
    session_arg: &str,
    cwd: &str,
    confirm: &mut dyn FnMut(&str) -> bool,
    out: &mut dyn FnMut(&str),
) -> bool {
    out(&paint(
        YELLOW,
        &format!("This directory is the same git repository: {cwd}"),
    ));
    out(&paint(
        DIM,
        &format!("To copy it into a new session instead, use --fork '{session_arg}'."),
    ));
    confirm(REBIND_PROMPT)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Run {
        action: CrossProjectAction,
        asked: Vec<String>,
        out: String,
        err: String,
    }

    fn strip(line: &str) -> String {
        let mut plain = String::new();
        let mut rest = line;
        while let Some(start) = rest.find('\x1b') {
            plain.push_str(&rest[..start]);
            let tail = &rest[start..];
            rest = tail.find('m').map_or("", |end| &tail[end + 1..]);
        }
        plain.push_str(rest);
        plain
    }

    fn run(repository_match: RepositoryMatch, interactive: bool, answer: bool) -> Run {
        let (mut asked, mut out, mut err) = (Vec::new(), Vec::new(), Vec::new());
        let action = choose_cross_project_action(CrossProjectChoice {
            session_arg: "0197f6e4",
            session_cwd: "/work/old/repo",
            cwd: "/work/new/repo",
            repository_match,
            interactive,
            confirm: &mut |message: &str| {
                asked.push(message.to_owned());
                answer
            },
            out: &mut |line: &str| out.push(strip(line)),
            err: &mut |line: &str| err.push(strip(line)),
        });
        Run {
            action,
            asked,
            out: out.join("\n"),
            err: err.join("\n"),
        }
    }

    #[test]
    fn offers_to_rebind_a_session_of_the_same_repository_and_shows_both_paths() {
        let accepted = run(RepositoryMatch::Same, true, true);
        assert_eq!(accepted.action, CrossProjectAction::Rebind);
        assert_eq!(accepted.asked, [REBIND_PROMPT]);
        assert!(
            accepted
                .out
                .contains("Session found in different project: /work/old/repo")
        );
        assert!(
            accepted
                .out
                .contains("This directory is the same git repository: /work/new/repo")
        );
        assert!(accepted.out.contains("--fork '0197f6e4'"));
        assert_eq!(
            run(RepositoryMatch::Same, true, false).action,
            CrossProjectAction::Abort
        );
    }

    #[test]
    fn keeps_the_fork_prompt_for_a_different_repository() {
        let result = run(RepositoryMatch::Different, true, true);
        assert_eq!(result.action, CrossProjectAction::Fork);
        assert_eq!(result.asked, [FORK_PROMPT]);
        assert!(!result.out.contains("--rebind"));
        assert_eq!(
            run(RepositoryMatch::Different, true, false).action,
            CrossProjectAction::Abort
        );
    }

    #[test]
    fn points_an_unrecognised_session_at_rebind_but_still_asks_to_fork() {
        let result = run(RepositoryMatch::Unknown, true, true);
        assert_eq!(result.action, CrossProjectAction::Fork);
        assert_eq!(result.asked, [FORK_PROMPT]);
        assert!(result.out.contains("--rebind '0197f6e4'"));
    }

    #[test]
    fn never_asks_without_an_interactive_session_and_prints_the_exact_commands() {
        let same = run(RepositoryMatch::Same, false, true);
        let different = run(RepositoryMatch::Different, false, true);
        let unknown = run(RepositoryMatch::Unknown, false, true);
        for result in [&same, &different, &unknown] {
            assert_eq!(result.action, CrossProjectAction::Fail);
            assert!(result.asked.is_empty());
            assert!(
                result
                    .err
                    .contains("Session found in different project: /work/old/repo")
            );
            assert!(result.err.contains("--fork '0197f6e4'"));
        }
        assert!(same.err.contains("--rebind '0197f6e4'"));
        assert!(unknown.err.contains("--rebind '0197f6e4'"));
        assert!(!different.err.contains("--rebind"));
    }
}
