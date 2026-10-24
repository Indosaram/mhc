//! Bun's launch globals behind the execution barrier (senpi `tool-child/gated-bun.ts`; senpi#3566, plan todo 4; DESIGN
//! PART 1 per-API table).
//!
//! mhc has no `Bun` global to patch; the same three launch APIs are a native gate over `std::process::Command`:
//! `spawn` without `detached` and with `detached: true` leads its own group through the POSIX shim, `spawn_sync` and
//! `shell` (`Bun.$`) stay in the child's ledgered group. The worker blocks until the ack in every case, so the real
//! call and every stdin write happen only after the server recorded the launch.
//!
//! Upstream's `bypass` (a node:child_process launch that Bun routes through `Bun.spawn` must not be gated twice) has no
//! counterpart: `GatedChildProcess` here calls `std::process` directly, never this gate, so one launch is one gate.
use std::path::{Path, PathBuf};

use crate::gated_child_process::{
    GatedSpawn, GatedSpawnError, SharedClient, SpawnOptions, SyncOutput, inherited_spawn,
    own_group_spawn, run_inherited_sync,
};
use crate::go_shim::ErrnoException;
use crate::launch_env_names::process_env;
use crate::protocol::{LaunchApi, LaunchContainment, LaunchLifetime};

/// The Bun launch globals, gated.
#[derive(Clone)]
pub struct GatedBun {
    client: SharedClient,
    posix: bool,
}

/// `installGatedBun(bun, client)`: `posix` is `process.platform !== "win32"`, injectable so both are testable anywhere.
pub fn install_gated_bun(client: SharedClient, posix: bool) -> GatedBun {
    GatedBun { client, posix }
}

/// Bun.spawn without an `env` hands the child the environment the process STARTED with; passing the current one keeps
/// every launch API equal: the tool child's scrubbed internals stay out.
fn with_current_env(options: &SpawnOptions) -> SpawnOptions {
    let mut options = options.clone();
    if options.env.is_none() {
        options.env = Some(process_env());
    }
    options
}

/// The error an ungated Bun.spawn throws for a program it cannot find.
pub fn bun_enoent_error(program: &str) -> ErrnoException {
    ErrnoException {
        message: format!("ENOENT: no such file or directory, posix_spawn '{program}'"),
        errno: -2,
        code: "ENOENT".to_string(),
        syscall: "posix_spawn".to_string(),
        path: program.to_string(),
        spawnargs: Vec::new(),
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// `Bun.which(program, { PATH, cwd })`: the file a program name resolves to, if any.
pub fn which(program: &str, path_env: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let candidate = match cwd {
            Some(cwd) if Path::new(program).is_relative() => cwd.join(program),
            _ => PathBuf::from(program),
        };
        return is_executable(&candidate).then_some(candidate);
    }
    std::env::split_paths(path_env)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

impl GatedBun {
    /// `Bun.spawn`.
    pub fn spawn(
        &self,
        cmd: &[String],
        options: &SpawnOptions,
    ) -> Result<GatedSpawn, GatedSpawnError> {
        if !self.posix || options.detached == Some(false) {
            return inherited_spawn(
                &self.client,
                LaunchApi::BunSpawn,
                LaunchContainment::Inherited,
                cmd.first().map_or("", String::as_str),
                cmd.get(1..).unwrap_or(&[]),
                &with_current_env(options),
            );
        }
        let program = cmd.first().map_or("", String::as_str);
        // A program Bun cannot find is refused here, before anything starts, exactly as an ungated Bun.spawn throws.
        let path_env = match &options.env {
            Some(env) => env.get("PATH").cloned().unwrap_or_default(),
            None => std::env::var("PATH").unwrap_or_default(),
        };
        if which(program, &path_env, options.cwd.as_deref()).is_none() {
            return Err(bun_enoent_error(program).into());
        }
        let lifetime = if options.detached == Some(true) {
            LaunchLifetime::Detached
        } else {
            LaunchLifetime::Owned
        };
        let mut options = with_current_env(options);
        options.shell = None;
        own_group_spawn(
            &self.client,
            LaunchApi::BunSpawn,
            program,
            cmd.get(1..).unwrap_or(&[]),
            &options,
            lifetime,
        )
    }

    /// `Bun.spawnSync`.
    pub fn spawn_sync(
        &self,
        cmd: &[String],
        options: &SpawnOptions,
    ) -> Result<SyncOutput, GatedSpawnError> {
        run_inherited_sync(
            &self.client,
            LaunchApi::BunSpawnSync,
            LaunchContainment::Inherited,
            cmd.first().map_or("", String::as_str),
            cmd.get(1..).unwrap_or(&[]),
            &with_current_env(options),
        )
    }

    /// Bun's `$` shell: the interpolated line runs through the platform shell in the child's own group.
    pub fn shell(&self, line: &str, options: &SpawnOptions) -> Result<SyncOutput, GatedSpawnError> {
        let (program, args): (&str, [&str; 2]) = if self.posix {
            ("/bin/sh", ["-c", line])
        } else {
            ("cmd.exe", ["/c", line])
        };
        let args = args.map(String::from);
        run_inherited_sync(
            &self.client,
            LaunchApi::BunShell,
            LaunchContainment::Inherited,
            program,
            &args,
            &with_current_env(options),
        )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::barrier_clock::BarrierClock;
    use crate::launch_barrier::StdioKind;
    use crate::worker_launch::{
        WorkerLaunchClient, WorkerLaunchClientOptions, WorkerLaunchMessage,
    };
    use std::sync::{Arc, Mutex};

    fn rig() -> (GatedBun, Arc<Mutex<Vec<WorkerLaunchMessage>>>) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&messages);
        let client = Arc::new(Mutex::new(WorkerLaunchClient::new(
            WorkerLaunchClientOptions {
                clock: BarrierClock::create(4).expect("clock"),
                post: Box::new(move |message| sink.lock().expect("lock").push(message)),
                job_id: Box::new(|| "j1".to_string()),
                skip_ack_wait: true,
                ack_timeout_ms: Some(5_000),
                worker_id: None,
            },
        )));
        (install_gated_bun(client, true), messages)
    }

    fn launches(
        messages: &Arc<Mutex<Vec<WorkerLaunchMessage>>>,
    ) -> Vec<(LaunchApi, LaunchContainment, LaunchLifetime)> {
        messages
            .lock()
            .expect("lock")
            .iter()
            .filter_map(|message| match message {
                WorkerLaunchMessage::Launch { request, .. } => {
                    Some((request.api, request.containment, request.lifetime))
                }
                _ => None,
            })
            .collect()
    }

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn gates_spawn_own_group_detached_spawn_sync_and_shell_with_their_containment() {
        let (bun, messages) = rig();
        let quiet = SpawnOptions {
            stdio: [StdioKind::Ignore; 3],
            ..SpawnOptions::default()
        };
        assert_eq!(
            bun.spawn(&cmd(&["true"]), &quiet)
                .expect("spawn")
                .wait()
                .exit_code,
            Some(0)
        );
        let detached = SpawnOptions {
            detached: Some(true),
            ..quiet.clone()
        };
        assert_eq!(
            bun.spawn(&cmd(&["true"]), &detached)
                .expect("detached")
                .wait()
                .exit_code,
            Some(0)
        );
        assert_eq!(
            bun.spawn_sync(&cmd(&["true"]), &SpawnOptions::default())
                .expect("sync")
                .status,
            Some(0)
        );
        assert_eq!(
            bun.shell("exit 0", &SpawnOptions::default())
                .expect("shell")
                .status,
            Some(0)
        );
        use LaunchApi::{BunShell, BunSpawn, BunSpawnSync};
        use LaunchContainment::{Inherited, OwnGroup};
        use LaunchLifetime::{Detached, Owned};
        assert_eq!(
            launches(&messages),
            vec![
                (BunSpawn, OwnGroup, Owned),
                (BunSpawn, OwnGroup, Detached),
                (BunSpawnSync, Inherited, Owned),
                (BunShell, Inherited, Owned)
            ]
        );
    }

    #[test]
    fn refuses_a_missing_program_before_anything_starts() {
        let (bun, messages) = rig();
        let quiet = SpawnOptions {
            stdio: [StdioKind::Ignore; 3],
            ..SpawnOptions::default()
        };
        let error = match bun.spawn(&cmd(&["definitely-not-a-program-xyz"]), &quiet) {
            Err(error) => error,
            Ok(_) => panic!("a missing program must be refused"),
        };
        assert_eq!(
            error.to_string(),
            "ENOENT: no such file or directory, posix_spawn 'definitely-not-a-program-xyz'"
        );
        assert!(launches(&messages).is_empty());
    }

    #[test]
    fn an_explicit_non_detached_spawn_stays_in_the_childs_group() {
        let (bun, messages) = rig();
        let options = SpawnOptions {
            stdio: [StdioKind::Ignore; 3],
            detached: Some(false),
            ..SpawnOptions::default()
        };
        bun.spawn(&cmd(&["true"]), &options).expect("spawn").wait();
        assert_eq!(
            launches(&messages),
            vec![(
                LaunchApi::BunSpawn,
                LaunchContainment::Inherited,
                LaunchLifetime::Owned
            )]
        );
    }
}
