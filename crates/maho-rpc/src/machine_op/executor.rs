//! Port of `packages/coding-agent/src/modes/rpc/machine-op/executor.ts`.
//!
//! One served op under the scope the relay host decided: confined gives `machine-ops` its root,
//! unconfined anchors relative paths at `root` and maps OS permission refusals (`EACCES`, `EPERM`,
//! `EROFS`) to `os_permission_denied`.

use crate::machine_op::op_registry::{OpCall, served_op};
use crate::machine_op::real_path::resolve_lexical;
use crate::machine_op::terminal_ops::TerminalOps;
use crate::machine_op::wire::{Args, MachineOpRequest, OpError, OpOutcome};
use maho_machine_ops::{
    AbortSignal, MachineOpError, MachineOps, OpContext, OpErrorCode, OpPrincipal, OpScope,
    PolicyHooks, PrincipalKind,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ERROR_MESSAGE_MAX: usize = 1_024;
const OS_REFUSALS: &[&str] = &["EACCES", "EPERM", "EROFS"];

fn truncate_message(msg: &str) -> String {
    if msg.len() <= ERROR_MESSAGE_MAX {
        return msg.to_owned();
    }
    msg.chars().take(ERROR_MESSAGE_MAX).collect()
}

/// Extract `(syscall, path)` from `MachineOpError::from_io`'s formatted message:
/// `"{code}: {io_err}, {syscall} '{path}'"` or `"{code}: {io_err}, '{path}'"`.
fn parse_io_details(message: &str) -> (Option<&str>, Option<&str>) {
    let Some(quote_end) = message.rfind('\'') else {
        return (None, None);
    };
    let Some(quote_start) = message[..quote_end].rfind('\'') else {
        return (None, None);
    };
    let path = Some(&message[quote_start + 1..quote_end]);
    let prefix = message[..quote_start].trim_end();
    let syscall = prefix
        .rsplit_once(", ")
        .map(|(_, verb)| verb.trim())
        .filter(|verb| !verb.is_empty());
    (syscall, path)
}

fn os_refusal(error: &MachineOpError) -> Option<OpError> {
    let errno = error.fs_code.as_deref()?;
    if !OS_REFUSALS.contains(&errno) {
        return None;
    }
    let (syscall, path) = parse_io_details(&error.message);
    let what = path.map_or_else(String::new, |p| format!(" {p}"));
    let verb = syscall.unwrap_or("access");
    let message = format!(
        "{errno}: this device's OS user may not {verb}{what}. Use a path that user can access, or change its permissions on that device."
    );
    Some(OpError {
        code: "os_permission_denied".to_owned(),
        message: truncate_message(&message),
    })
}

fn resolve_str(root: &str, raw: &str) -> String {
    resolve_lexical(Path::new(root), Path::new(raw))
        .to_string_lossy()
        .into_owned()
}

fn anchored(op: &str, args: &Args, root: &str) -> Args {
    if !op.starts_with("fs.") {
        return args.clone();
    }
    let mut out = args.clone();
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        out.insert("path".to_owned(), Value::String(resolve_str(root, path)));
    }
    if op == "fs.glob" {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        out.insert("cwd".to_owned(), Value::String(resolve_str(root, cwd)));
    }
    out
}

fn failure(error: &MachineOpError, confined: bool) -> OpOutcome {
    if !confined && let Some(os) = os_refusal(error) {
        return OpOutcome::failed(os.code, os.message);
    }
    let message = truncate_message(&error.message);
    if error.code == OpErrorCode::Aborted {
        OpOutcome::cancelled("aborted", message)
    } else {
        OpOutcome::failed(error.code.as_str(), message)
    }
}

pub struct ExecutorDeps<'a> {
    pub ops: &'a Arc<dyn MachineOps>,
    pub terminal: &'a TerminalOps,
    pub pid_of: &'a (dyn Fn(&str) -> Option<u32> + Send + Sync),
    pub owner_of: &'a (dyn Fn(&str) -> Option<String> + Send + Sync),
}

fn foreign_handle<'a>(
    request: &'a MachineOpRequest,
    handle_arg: Option<&str>,
    deps: &ExecutorDeps<'_>,
) -> Option<&'a str> {
    let arg_name = handle_arg?;
    let handle = request.args.get(arg_name).and_then(Value::as_str)?;
    if request.cross_owner {
        return None;
    }
    let owner = (deps.owner_of)(handle)?;
    if owner != request.sender {
        Some(handle)
    } else {
        None
    }
}

pub async fn run_machine_op(
    request: &MachineOpRequest,
    signal: &AbortSignal,
    deps: &ExecutorDeps<'_>,
) -> OpOutcome {
    let Some(entry) = served_op(&request.op) else {
        return OpOutcome::refused("malformed", format!("unknown op {}", request.op));
    };
    if let Some(foreign) = foreign_handle(request, entry.handle_arg, deps) {
        return OpOutcome::failed(
            "not_owner",
            format!("handle {foreign} belongs to another sender"),
        );
    }
    let root = request.scope.root.as_str();
    let confined = request.scope.confined;
    let ctx = OpContext {
        scope: OpScope {
            id: format!("{}/omo", request.sender),
            root: confined.then(|| PathBuf::from(root)),
        },
        principal: OpPrincipal {
            kind: PrincipalKind::RemoteDevice,
            id: request.sender.clone(),
        },
        signal: signal.clone(),
        limits: deps.ops.limits(),
        policy: PolicyHooks::default(),
        audit: None,
        path_lock: None,
    };
    let effective_args = if confined {
        request.args.clone()
    } else {
        anchored(&request.op, &request.args, root)
    };
    let call = OpCall {
        ctx: &ctx,
        ops: deps.ops,
        root,
        confined,
        env_overlay: &request.env_overlay,
        sender: &request.sender,
        signal,
        terminal: deps.terminal,
        pid_of: deps.pid_of,
    };
    match entry.run(&effective_args, &call).await {
        Ok(outcome) => outcome,
        Err(err) => failure(&err, confined),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_op::live_handle::tokio_after;
    use crate::machine_op::machine_terminals::MachineTerminals;
    use crate::machine_op::wire::{OpScope, OpState};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use maho_machine_ops::{
        DEFAULT_LIMITS, HeldTerminal, HolderLink, HolderSpawnRequest, LocalMachineOpsOptions,
        OpFuture, create_local_machine_ops,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    struct DummyHolder;
    impl HolderLink for DummyHolder {
        fn spawn(&self, _req: HolderSpawnRequest) -> OpFuture<'_, Arc<dyn HeldTerminal>> {
            Box::pin(async { Err(MachineOpError::new(OpErrorCode::InvalidArgument, "unused")) })
        }
        fn close(&self) -> OpFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn anchors_unconfined_fs_paths_and_enforces_confined_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let work = temp.path().join("work");
        let outside = temp.path().join("outside.txt");
        std::fs::create_dir_all(&work).expect("mkdir");
        std::fs::write(&outside, "secret").expect("write outside");

        let ops = create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: Some(temp.path().to_path_buf()),
            holder: Arc::new(DummyHolder),
            limits: Some(DEFAULT_LIMITS),
            kill_process_tree: None,
            resolve_fd: None,
        });
        let terminals = MachineTerminals::new(tokio_after());
        let term_ops = TerminalOps::new(Arc::clone(&terminals), tokio_after());
        let deps = ExecutorDeps {
            ops: &ops,
            terminal: &term_ops,
            pid_of: &|_| None,
            owner_of: &|_| None,
        };

        let write_req = MachineOpRequest {
            op: "fs.write".into(),
            args: json!({ "path": "rel.txt", "bytes": STANDARD.encode("hello") })
                .as_object()
                .expect("obj")
                .clone(),
            op_id: "op-w".into(),
            deadline_ms: None,
            scope: OpScope {
                root: work.to_string_lossy().into_owned(),
                confined: false,
            },
            sender: "dev-1".into(),
            env_overlay: BTreeMap::new(),
            cross_owner: false,
        };
        let written = run_machine_op(&write_req, &AbortSignal::default(), &deps).await;
        assert_eq!(written.state, OpState::Applied);
        assert_eq!(
            written.response.expect("resp")["path"],
            json!(work.join("rel.txt").to_string_lossy())
        );

        let escape_req = MachineOpRequest {
            op: "fs.read".into(),
            args: json!({ "path": outside.to_string_lossy() })
                .as_object()
                .expect("obj")
                .clone(),
            op_id: "op-r".into(),
            deadline_ms: None,
            scope: OpScope {
                root: work.to_string_lossy().into_owned(),
                confined: true,
            },
            sender: "dev-1".into(),
            env_overlay: BTreeMap::new(),
            cross_owner: false,
        };
        let denied = run_machine_op(&escape_req, &AbortSignal::default(), &deps).await;
        assert_eq!(denied.state, OpState::Failed);
        assert_eq!(denied.error.expect("err").code, "policy_denied");

        terminals.dispose().await;
    }

    #[test]
    fn maps_unconfined_eacces_to_os_permission_denied() {
        let io_err = std::io::Error::from_raw_os_error(libc::EACCES);
        let mop_err =
            MachineOpError::from_io(&io_err, Some("open"), Some(Path::new("/locked/x.txt")));
        let outcome = failure(&mop_err, false);
        assert_eq!(outcome.state, OpState::Failed);
        let err = outcome.error.expect("err");
        assert_eq!(err.code, "os_permission_denied");
        assert!(
            err.message
                .contains("EACCES: this device's OS user may not open /locked/x.txt.")
        );
    }
}
