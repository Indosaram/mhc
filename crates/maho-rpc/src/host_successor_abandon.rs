//! Undoing a refused handoff (senpi#4602 split this out of `host-successor.ts`): the successor is
//! killed, its generation is released once it has exited, and the boot `settings.json` it overwrote
//! is put back byte for byte.

use crate::host_daemon_paths::{HOST_STATE_FILE_MODE, HostDaemonPaths, generation_paths};
use crate::host_daemon_registration::release_generation;
use crate::host_ensure_stop::{
    StopSignal, StopTarget, announce_stop, record_escalation, signal_pid,
};
use crate::host_stop_intent::HostStopSender;
use std::future::Future;
use std::time::Duration;

/// How long a refused handoff waits for the successor it killed to exit before releasing its record.
const SUCCESSOR_EXIT_WAIT_MS: u64 = 5_000;

pub struct AbandonSuccessorContext<'a, E>
where
    E: Future<Output = ()>,
{
    pub child: Option<&'a mut tokio::process::Child>,
    pub exited: Option<E>,
    pub paths: &'a HostDaemonPaths,
    pub instance_id: &'a str,
    pub boot_settings: Option<&'a str>,
    /// Why the successor is being killed: the stop intent and the record name it.
    pub reason: &'a str,
}

/// Undoes what a refused handoff left: kills the successor, releases its generation once it has
/// exited (a successor that never got a pid never ran, so its directory goes at once; one that
/// outlives the wait keeps its record until pruning finds it dead), and puts the boot settings back
/// as they were. A cleanup that fails is answered as a string, never thrown.
pub async fn abandon_successor<E>(context: AbandonSuccessorContext<'_, E>) -> Option<String>
where
    E: Future<Output = ()>,
{
    let target = StopTarget {
        daemon_dir: context.paths.dir.clone(),
        generation: generation_paths(context.paths, context.instance_id),
        instance_id: context.instance_id.to_owned(),
        sender: HostStopSender {
            pid: std::process::id(),
            kind: "successor".to_owned(),
            generation: None,
        },
        reason: context.reason.to_owned(),
    };

    let (pid, still_running) = match context.child {
        Some(child) => {
            let pid = child.id();
            let running = pid.is_some() && matches!(child.try_wait(), Ok(None));
            (pid, running)
        }
        None => (None, false),
    };

    let killed = if still_running && let Some(pid) = pid {
        let announced = announce_stop(&target, pid);
        let _ = signal_pid(pid, StopSignal::Kill);
        Some(announced)
    } else {
        None
    };

    let result: std::io::Result<()> = async {
        match context.boot_settings {
            None => match std::fs::remove_file(&context.paths.settings_file) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            },
            Some(contents) => {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(HOST_STATE_FILE_MODE);
                }
                let mut file = options.open(&context.paths.settings_file)?;
                file.write_all(contents.as_bytes())?;
            }
        }
        match pid {
            None => match std::fs::remove_dir_all(
                generation_paths(context.paths, context.instance_id).dir,
            ) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            },
            Some(pid) => {
                if let Some(exited) = context.exited
                    && tokio::time::timeout(Duration::from_millis(SUCCESSOR_EXIT_WAIT_MS), exited)
                        .await
                        .is_ok()
                {
                    if let Some(announced) = killed.as_ref() {
                        record_escalation(&target, announced);
                    }
                    release_generation(context.paths, context.instance_id, pid)?;
                }
            }
        }
        Ok(())
    }
    .await;

    result
        .err()
        .map(|cause| format!("successor cleanup failed: {cause}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_daemon_paths::{
        create_daemon_directories, create_generation_directory, create_host_daemon_paths,
    };

    #[tokio::test]
    async fn unspawned_successor_removes_generation_and_restores_boot_settings() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = create_host_daemon_paths("/tmp/s.sock", temp.path());
        create_daemon_directories(&paths).expect("dirs");
        let gen_paths = generation_paths(&paths, "succ-1");
        create_generation_directory(&gen_paths).expect("gen dir");
        std::fs::write(&paths.settings_file, "{\"new\":true}").expect("write settings");

        let err = abandon_successor(AbandonSuccessorContext {
            child: None,
            exited: None::<std::future::Ready<()>>,
            paths: &paths,
            instance_id: "succ-1",
            boot_settings: Some("{\"old\":true}"),
            reason: "refused",
        })
        .await;
        assert_eq!(err, None);
        assert!(!gen_paths.dir.exists());
        assert_eq!(
            std::fs::read_to_string(&paths.settings_file).expect("read"),
            "{\"old\":true}"
        );

        // Restoring `None` deletes settings.json.
        let err2 = abandon_successor(AbandonSuccessorContext {
            child: None,
            exited: None::<std::future::Ready<()>>,
            paths: &paths,
            instance_id: "succ-1",
            boot_settings: None,
            reason: "refused",
        })
        .await;
        assert_eq!(err2, None);
        assert!(!paths.settings_file.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_successor_is_killed_recorded_and_released() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = create_host_daemon_paths("/tmp/s.sock", temp.path());
        create_daemon_directories(&paths).expect("dirs");
        let gen_paths = generation_paths(&paths, "succ-2");
        create_generation_directory(&gen_paths).expect("gen dir");
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn");
        let pid = child.id().expect("pid");
        std::fs::write(&gen_paths.pid_file, format!("{{\"pid\":{pid}}}")).expect("pidfile");

        let err = abandon_successor(AbandonSuccessorContext {
            child: Some(&mut child),
            exited: Some(async {
                // Reaped via wait; we pass a future that polls wait on a clone or wait outside.
            }),
            paths: &paths,
            instance_id: "succ-2",
            boot_settings: None,
            reason: "handoff_timeout",
        })
        .await;
        assert_eq!(err, None);
        let _ = child.wait().await;
        assert!(!gen_paths.dir.exists());
        assert!(!crate::host_crash_record::read_host_crash_records(&paths.dir).is_empty());
    }
}
