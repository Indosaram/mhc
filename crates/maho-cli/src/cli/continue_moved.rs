//! Port of senpi `cli/continue-moved.ts` (pin 903b8336).
//!
//! `--continue` in a project with no session of its own: the newest session this repository
//! recorded before it moved is offered for the #2182 rebind. Returns its path when the user
//! accepts. Without an interactive session nothing is asked; the `--rebind` command is printed
//! instead.
//!
//! senpi calls core `listMovedSessions(cwd, options)` itself. That module has no Rust port yet, so
//! the caller passes the listing (newest first) as `list_moved`. senpi's async `confirm` is a
//! blocking callback here, as in `cross_project_session`.

use std::future::Future;

use maho_core::session_discovery::SessionInfo;

use super::cross_project_session::confirm_same_repository_rebind;

const YELLOW: (&str, &str) = ("\x1b[33m", "\x1b[39m");

fn yellow(text: &str) -> String {
    format!("{}{text}{}", YELLOW.0, YELLOW.1)
}

/// senpi `ContinueMovedChoice` (minus `MovedSessionOptions`, which belong to the lister).
pub struct ContinueMovedChoice<'a> {
    pub cwd: &'a str,
    pub interactive: bool,
    pub confirm: &'a mut dyn FnMut(&str) -> bool,
    pub out: &'a mut dyn FnMut(&str),
    pub err: &'a mut dyn FnMut(&str),
}

/// senpi `movedSessionToContinue`. `list_moved` is senpi `listMovedSessions(cwd, choice)`.
pub async fn moved_session_to_continue<F, Fut>(
    choice: ContinueMovedChoice<'_>,
    list_moved: F,
) -> Option<String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Vec<SessionInfo>>,
{
    let newest = list_moved().await.into_iter().next()?;
    if !choice.interactive {
        (choice.err)(&yellow(&format!(
            "A session of this repository was recorded at {} before it moved. Use --rebind '{}' to continue it here.",
            newest.cwd, newest.id
        )));
        return None;
    }
    (choice.out)(&yellow(&format!(
        "Session found in different project: {}",
        newest.cwd
    )));
    let accepted =
        confirm_same_repository_rebind(&newest.id, choice.cwd, choice.confirm, choice.out);
    accepted.then_some(newest.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::cross_project_session::REBIND_PROMPT;
    use chrono::{TimeZone, Utc};

    fn session(id: &str, cwd: &str, path: &str) -> SessionInfo {
        let at = Utc.timestamp_millis_opt(0).single().expect("epoch");
        SessionInfo {
            path: path.to_owned(),
            id: id.to_owned(),
            cwd: cwd.to_owned(),
            name: None,
            parent_session_path: None,
            created: at,
            modified: at,
            message_count: 0,
            first_message: String::new(),
            all_messages_text: String::new(),
        }
    }

    struct Run {
        result: Option<String>,
        asked: Vec<String>,
        out: Vec<String>,
        err: Vec<String>,
    }

    fn run(listed: Vec<SessionInfo>, interactive: bool, answer: bool) -> Run {
        let (mut asked, mut out, mut err) = (Vec::new(), Vec::new(), Vec::new());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = runtime.block_on(moved_session_to_continue(
            ContinueMovedChoice {
                cwd: "/work/new/repo",
                interactive,
                confirm: &mut |message: &str| {
                    asked.push(message.to_owned());
                    answer
                },
                out: &mut |line: &str| out.push(line.to_owned()),
                err: &mut |line: &str| err.push(line.to_owned()),
            },
            || std::future::ready(listed),
        ));
        Run {
            result,
            asked,
            out,
            err,
        }
    }

    fn moved() -> Vec<SessionInfo> {
        vec![
            session("newest", "/work/old/repo", "/sessions/old/newest.jsonl"),
            session("older", "/work/old/repo", "/sessions/old/older.jsonl"),
        ]
    }

    #[test]
    fn nothing_moved_returns_none_without_output() {
        let run = run(Vec::new(), true, true);
        assert_eq!(run.result, None);
        assert!(run.asked.is_empty() && run.out.is_empty() && run.err.is_empty());
    }

    #[test]
    fn interactive_accept_returns_the_newest_moved_session_path() {
        let run = run(moved(), true, true);
        assert_eq!(run.result.as_deref(), Some("/sessions/old/newest.jsonl"));
        assert_eq!(run.asked, [REBIND_PROMPT]);
        assert_eq!(
            run.out[0],
            "\x1b[33mSession found in different project: /work/old/repo\x1b[39m"
        );
        assert!(run.out.iter().any(|line| line.contains("--fork 'newest'")));
        assert!(run.err.is_empty());
    }

    #[test]
    fn interactive_decline_returns_none() {
        let run = run(moved(), true, false);
        assert_eq!(run.result, None);
        assert_eq!(run.asked, [REBIND_PROMPT]);
    }

    #[test]
    fn non_interactive_prints_the_rebind_command_and_never_asks() {
        let run = run(moved(), false, true);
        assert_eq!(run.result, None);
        assert!(run.asked.is_empty() && run.out.is_empty());
        assert_eq!(
            run.err,
            [
                "\x1b[33mA session of this repository was recorded at /work/old/repo before it moved. Use --rebind 'newest' to continue it here.\x1b[39m"
            ]
        );
    }
}
