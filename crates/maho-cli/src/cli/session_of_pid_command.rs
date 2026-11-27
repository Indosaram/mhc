//! Port of `packages/coding-agent/src/cli/session-of-pid-command.ts` (senpi#6972).

use maho_core::session_spawn_attestation::{PidSession, resolve_pid_session};

use super::print_then_exit::exit_after_output;

/// `session-of-pid <pid> --json`: which session spawned `pid`, from the kernel's ancestry and the
/// spawning engine's records under this agent directory, never from an environment. One version-1
/// JSON record; exit 0 when the pid is attested, 1 when it is not, 2 on invalid arguments.
/// A metadata command: it starts no host and writes nothing.
pub fn dispatch_session_of_pid_command(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("session-of-pid") {
        return Ok(());
    }
    let parsed_pid = args
        .get(1)
        .filter(|raw| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    let valid = args.len() == 3 && args.get(2).map(String::as_str) == Some("--json");
    let Some(pid) = parsed_pid.filter(|_| valid) else {
        let app = maho_core::config::app_name();
        let line = serde_json::json!({
            "version": 1,
            "ok": false,
            "code": "invalid_arguments",
            "message": format!("Usage: {app} session-of-pid <pid> --json"),
        });
        exit_after_output(&format!("{line}\n"), 2);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let answer = runtime.block_on(resolve_pid_session(pid, None));
    let (payload, exit_code) = format_pid_session_response(&answer);
    exit_after_output(&format!("{payload}\n"), exit_code);
}

pub fn format_pid_session_response(answer: &PidSession) -> (serde_json::Value, i32) {
    match answer {
        PidSession::Attested {
            pid,
            session_id,
            attested_pid,
            host_pid,
            chain,
        } => (
            serde_json::json!({
                "version": 1,
                "ok": true,
                "attested": true,
                "pid": pid,
                "sessionId": session_id,
                "attestedPid": attested_pid,
                "hostPid": host_pid,
                "chain": chain,
            }),
            0,
        ),
        PidSession::Unattested { pid, reason } => (
            serde_json::json!({
                "version": 1,
                "ok": true,
                "attested": false,
                "pid": pid,
                "reason": reason,
            }),
            1,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::session_spawn_attestation::PidSessionReason;

    #[test]
    fn formats_attested_and_unattested_json_records() {
        let (ok, code) = format_pid_session_response(&PidSession::Attested {
            pid: 42,
            session_id: "sess-1".into(),
            attested_pid: 40,
            host_pid: 10,
            chain: vec![42, 40],
        });
        assert_eq!(code, 0);
        assert_eq!(ok["version"], 1);
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["attested"], true);
        assert_eq!(ok["sessionId"], "sess-1");

        let (no, code) = format_pid_session_response(&PidSession::Unattested {
            pid: 99,
            reason: PidSessionReason::NoAttestedAncestor,
        });
        assert_eq!(code, 1);
        assert_eq!(no["attested"], false);
        assert_eq!(no["reason"], "no_attested_ancestor");
    }
}
