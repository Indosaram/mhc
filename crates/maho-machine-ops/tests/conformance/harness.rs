use maho_machine_ops::{
    AbortSignal, DEFAULT_LIMITS, Limits, MachineOps, OpContext, OpPrincipal, OpScope, PolicyHooks,
    PrincipalKind, create_local_machine_ops,
    process::in_process_holder::create_in_process_holder_link,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

pub const INSIDE_TEXT: &str = "hello world\nsecond line\n";
pub const SECRET_TEXT: &str = "secret\n";
pub const READ_LIMIT: usize = 10 * 1024 * 1024;

pub struct Fixture {
    _temp: TempDir,
    pub base: PathBuf,
    pub root: PathBuf,
    pub outside: PathBuf,
    pub state_dir: PathBuf,
    pub ops: Arc<dyn MachineOps>,
}

impl Fixture {
    pub async fn create(with_state_dir: bool) -> Self {
        let temp = tempfile::tempdir().expect("create temp dir");
        let base = tokio::fs::canonicalize(temp.path())
            .await
            .expect("canonicalize tempdir");
        let root = base.join("root");
        let outside = base.join("outside");
        let state_dir = base.join("state");
        tokio::fs::create_dir_all(root.join("dir"))
            .await
            .expect("mkdir root/dir");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("mkdir outside");
        tokio::fs::write(root.join("inside.txt"), INSIDE_TEXT)
            .await
            .expect("write inside.txt");
        tokio::fs::write(root.join("dir").join("a.ts"), "export const a = 1;\n")
            .await
            .expect("write dir/a.ts");
        tokio::fs::write(outside.join("secret.txt"), SECRET_TEXT)
            .await
            .expect("write secret.txt");
        #[cfg(unix)]
        tokio::fs::symlink(&outside, root.join("escape"))
            .await
            .expect("symlink escape");

        let holder = create_in_process_holder_link();
        let ops = create_local_machine_ops(maho_machine_ops::LocalMachineOpsOptions {
            state_dir: with_state_dir.then(|| state_dir.clone()),
            holder,
            limits: None,
            kill_process_tree: None,
            resolve_fd: None,
        });

        Self {
            _temp: temp,
            base,
            root,
            outside,
            state_dir,
            ops,
        }
    }

    pub fn ctx(&self) -> OpContext {
        OpContext {
            scope: OpScope {
                id: "conformance".to_owned(),
                root: Some(self.root.clone()),
            },
            principal: OpPrincipal {
                kind: PrincipalKind::RemoteDevice,
                id: "conformance-device".to_owned(),
            },
            signal: AbortSignal::default(),
            limits: DEFAULT_LIMITS,
            policy: PolicyHooks::default(),
            audit: None,
            path_lock: None,
        }
    }

    pub fn ctx_with(&self, policy: PolicyHooks, signal: Option<AbortSignal>) -> OpContext {
        OpContext {
            scope: OpScope {
                id: "conformance".to_owned(),
                root: Some(self.root.clone()),
            },
            principal: OpPrincipal {
                kind: PrincipalKind::RemoteDevice,
                id: "conformance-device".to_owned(),
            },
            signal: signal.unwrap_or_default(),
            limits: Limits::default(),
            policy,
            audit: None,
            path_lock: None,
        }
    }
}

pub async fn write_oversized_file(path: &Path) {
    let data = vec![b'a'; READ_LIMIT + 1];
    tokio::fs::write(path, data)
        .await
        .expect("write oversized file");
}

pub fn process_gone(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::errno::Errno;
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        let raw = i32::try_from(pid).unwrap_or(i32::MAX);
        matches!(kill(Pid::from_raw(raw), None), Err(Errno::ESRCH))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

pub async fn pid_gone_within(pid: u32, ms: u64) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
    loop {
        if process_gone(pid) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
