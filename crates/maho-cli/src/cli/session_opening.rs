//! Port of senpi `cli/session-opening.ts`.
//!
//! Startup admission before loading/migrating/running an existing session (senpi#5756, #6059),
//! plus interactive post-open foreign holder warning (`prepare_session_opening`).

use std::future::Future;
use std::pin::Pin;

use maho_core::foreign_session_holders::{
    CheckSessionHoldersOutcome, CheckedSessionHolder, SESSION_HOLDER_LOOKUP_FAILED,
    check_session_holders, describe_session_holders, host_attach_socket,
    session_holders_from_holders, session_id_from_file, stop_session_holders, take_over_refusal,
};
use maho_core::project_trust::AppMode;
use maho_core::schedule::native_probes::NativeProbes;
use maho_core::session_holder_records::SessionHolderProbes;

fn native_holder_probes() -> Option<NativeProbes> {
    NativeProbes::new(std::sync::Arc::new(|| {
        chrono::Utc::now().timestamp_millis().max(0) as u64
    }))
    .ok()
}

/// A live session on this machine's multi-session host that the terminal attaches to instead of opening it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostAttachTarget {
    pub socket: String,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupAdmission {
    Open,
    Cancelled,
    Refused,
    Attach(HostAttachTarget),
}

/// Exit code of a startup refused because another live process holds the session.
/// The schedule runner reads it to leave the occurrence pending instead of failing it (senpi#5756).
pub const SESSION_HELD_EXIT_CODE: i32 = 76;

const RETRY: &str = "then open it again";

pub type ConfirmFuture<'a> = Pin<Box<dyn Future<Output = bool> + 'a>>;
pub type LookupHoldersFuture<'a> = Pin<Box<dyn Future<Output = CheckSessionHoldersOutcome> + 'a>>;
pub type StopHoldersFuture<'a> = Pin<Box<dyn Future<Output = Result<bool, String>> + 'a>>;
pub type StopHoldersFn<'a> =
    &'a mut dyn FnMut(String, Vec<CheckedSessionHolder>) -> StopHoldersFuture<'a>;

pub struct StartupAdmissionIo<'a> {
    pub can_ask: bool,
    pub confirm: &'a mut dyn FnMut(String) -> ConfirmFuture<'a>,
    pub err: &'a mut dyn FnMut(&str),
    pub can_attach: bool,
    pub lookup: Option<&'a mut dyn FnMut(String) -> LookupHoldersFuture<'a>>,
    pub stop: Option<StopHoldersFn<'a>>,
}

/// Startup admission for a session another live process holds (senpi#5756, #6059): opening it
/// would start a second run of the same session. Without a person to ask it is refused with the
/// holder named; on a terminal the person chooses to cancel or to take over.
pub async fn admit_startup_session(
    session_file: Option<&str>,
    io: &mut StartupAdmissionIo<'_>,
) -> StartupAdmission {
    let Some(session_file) = session_file else {
        return StartupAdmission::Open;
    };
    let check = if let Some(lookup) = io.lookup.as_mut() {
        lookup(session_file.to_owned()).await
    } else if let Some(probes) = native_holder_probes() {
        check_session_holders(session_file, &probes).await
    } else {
        CheckSessionHoldersOutcome::LookupFailed
    };
    let holders = match check {
        CheckSessionHoldersOutcome::LookupFailed => {
            (io.err)(&format!(
                "\x1b[33mWarning: {SESSION_HOLDER_LOOKUP_FAILED}\x1b[39m"
            ));
            return StartupAdmission::Open;
        }
        CheckSessionHoldersOutcome::Holders(holders) => holders,
    };
    if holders.is_empty() {
        return StartupAdmission::Open;
    }
    let socket = if io.can_ask && io.can_attach {
        host_attach_socket(&holders)
    } else {
        None
    };
    let session_id = match &socket {
        Some(_) => session_id_from_file(session_file).ok().flatten(),
        None => None,
    };
    if let (Some(socket), Some(session_id)) = (socket, session_id) {
        return StartupAdmission::Attach(HostAttachTarget { socket, session_id });
    }
    let bare_holders = session_holders_from_holders(&holders);
    let held = format!(
        "This session is open in another process ({}); opening it here would start a second run of the same session.",
        describe_session_holders(&bare_holders)
    );
    let refusal = take_over_refusal(&holders, Some(RETRY));
    if !io.can_ask || refusal.is_some() {
        let tail = refusal.unwrap_or_else(|| format!("Quit it there, {RETRY}."));
        (io.err)(&format!("\x1b[31mNot opened: {held} {tail}\x1b[39m"));
        return StartupAdmission::Refused;
    }
    if !(io.confirm)(format!("{held}\nTake over (stop it there)?")).await {
        (io.err)(&format!(
            "Not opened: the session is open in another process ({}).",
            describe_session_holders(&bare_holders)
        ));
        return StartupAdmission::Cancelled;
    }
    let stop_result = if let Some(stop) = io.stop.as_mut() {
        stop(session_file.to_owned(), holders).await
    } else if let Some(native) = native_holder_probes() {
        let probes: &dyn SessionHolderProbes = &native;
        Ok(stop_session_holders(session_file, &holders, probes, None).await)
    } else {
        Err("Failed to initialize process probes".to_owned())
    };
    match stop_result {
        Err(error) => {
            (io.err)(&format!(
                "\x1b[31mNot opened: could not stop the other process ({error}).\x1b[39m"
            ));
            StartupAdmission::Refused
        }
        Ok(false) => {
            (io.err)(&format!(
                "\x1b[31mNot opened: the session is still open in another process; quit it there, {RETRY}.\x1b[39m"
            ));
            StartupAdmission::Refused
        }
        Ok(true) => StartupAdmission::Open,
    }
}

/// senpi `prepareSessionOpening`. `warn` receives the coloured `Warning: ...` line that senpi
/// prints with `console.error`.
pub async fn prepare_session_opening<F, Fut>(
    mode: AppMode,
    holder_warning: F,
    warn: &mut dyn FnMut(&str),
) where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Option<String>>,
{
    if mode != AppMode::Interactive {
        return;
    }
    if let Some(warning) = holder_warning().await {
        warn(&format!("\x1b[33mWarning: {warning}\x1b[39m"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mode: AppMode, warning: Option<&str>) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let mut asked = false;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(prepare_session_opening(
            mode,
            || {
                asked = true;
                std::future::ready(warning.map(str::to_owned))
            },
            &mut |line: &str| lines.push(line.to_owned()),
        ));
        (lines, asked)
    }

    #[test]
    fn an_interactive_launch_prints_the_holder_warning() {
        let (lines, asked) = run(AppMode::Interactive, Some("pid 7 holds this session"));
        assert!(asked);
        assert_eq!(lines, ["\x1b[33mWarning: pid 7 holds this session\x1b[39m"]);
        assert!(run(AppMode::Interactive, None).0.is_empty());
    }

    #[test]
    fn other_modes_never_look_up_holders() {
        for mode in [AppMode::Print, AppMode::Json, AppMode::Rpc] {
            let (lines, asked) = run(mode, Some("ignored"));
            assert!(!asked);
            assert!(lines.is_empty());
        }
    }

    #[tokio::test]
    async fn admits_refuses_attaches_and_takes_over_held_sessions() {
        use maho_core::session_holder_records::SessionHolder;

        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("sess.jsonl");
        std::fs::write(&file, r#"{"type":"session","id":"sess-abc","cwd":"/work"}"#)
            .expect("write");
        let file_str = file.to_string_lossy().into_owned();

        let held_holder = CheckedSessionHolder {
            holder: SessionHolder {
                pid: 9001,
                cwd: Some("/work".into()),
                host_socket: None,
                host_instance: None,
            },
            unproven: false,
        };

        // Non-interactive refuses held sessions with stderr explanation.
        let mut errors = Vec::new();
        {
            let mut confirm = |_: String| -> ConfirmFuture<'_> { Box::pin(async { true }) };
            let mut lookup = |_: String| -> LookupHoldersFuture<'_> {
                let h = held_holder.clone();
                Box::pin(async move { CheckSessionHoldersOutcome::Holders(vec![h]) })
            };
            let mut err = |line: &str| errors.push(line.to_owned());
            let mut io = StartupAdmissionIo {
                can_ask: false,
                confirm: &mut confirm,
                err: &mut err,
                can_attach: false,
                lookup: Some(&mut lookup),
                stop: None,
            };
            assert_eq!(
                admit_startup_session(Some(&file_str), &mut io).await,
                StartupAdmission::Refused
            );
        }
        assert!(errors.iter().any(|l| l.contains("pid 9001")));

        // Multi-session host holder attaches when interactive & can_attach.
        let host_holder = CheckedSessionHolder {
            holder: SessionHolder {
                pid: 9002,
                cwd: Some("/work".into()),
                host_socket: Some("/tmp/rpc.sock".into()),
                host_instance: Some("inst-1".into()),
            },
            unproven: false,
        };
        let mut errors = Vec::new();
        let mut confirm = |_: String| -> ConfirmFuture<'_> { Box::pin(async { true }) };
        let mut lookup = |_: String| -> LookupHoldersFuture<'_> {
            let h = host_holder.clone();
            Box::pin(async move { CheckSessionHoldersOutcome::Holders(vec![h]) })
        };
        let mut err = |line: &str| errors.push(line.to_owned());
        let mut io = StartupAdmissionIo {
            can_ask: true,
            confirm: &mut confirm,
            err: &mut err,
            can_attach: true,
            lookup: Some(&mut lookup),
            stop: None,
        };
        assert_eq!(
            admit_startup_session(Some(&file_str), &mut io).await,
            StartupAdmission::Attach(HostAttachTarget {
                socket: "/tmp/rpc.sock".into(),
                session_id: "sess-abc".into(),
            })
        );
    }
}
