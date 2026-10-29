use super::*;
use crate::ownership::EndpointIdentity;
use pretty_assertions::assert_eq;
use std::cell::RefCell;
use std::path::PathBuf;

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    own: DaemonPaths,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("daemon");
    let own = DaemonPaths::under_dir(base.join("v2.0.0"), "2.0.0");
    std::fs::create_dir_all(&own.dir).unwrap();
    Fixture {
        base,
        own,
        _root: root,
    }
}

fn owner(pid: u32) -> DaemonOwner {
    DaemonOwner {
        pid,
        nonce: "n".to_string(),
        started_at: "t".to_string(),
        endpoint: EndpointIdentity::Missing {
            path: "/tmp/old.sock".to_string(),
        },
    }
}

#[derive(Default)]
struct Recorder {
    signals: RefCell<Vec<(u32, ReapSignal)>>,
    removed: RefCell<Vec<PathBuf>>,
    logs: RefCell<Vec<String>>,
}

struct Scenario<'a> {
    owner: Option<DaemonOwner>,
    alive: &'a dyn Fn(u32) -> bool,
    attest: bool,
    exits_after: &'a dyn Fn(ReapSignal) -> bool,
    platform: ReapPlatform,
}

fn run(fixture: &Fixture, scenario: &Scenario<'_>, recorder: &Recorder) -> Vec<VersionReapResult> {
    let last_signal = RefCell::new(None);
    let read_owner = |_paths: &DaemonPaths| scenario.owner.clone();
    let attest = |_pid: u32, _platform: ReapPlatform| scenario.attest;
    let send_signal = |pid: u32, signal: ReapSignal| {
        recorder.signals.borrow_mut().push((pid, signal));
        *last_signal.borrow_mut() = Some(signal);
        true
    };
    let wait_for_exit = |_pid: u32, _timeout: u64| {
        last_signal
            .borrow()
            .is_some_and(|signal| (scenario.exits_after)(signal))
    };
    let remove_dir = |path: &Path| recorder.removed.borrow_mut().push(path.to_path_buf());
    let log = |message: &str| recorder.logs.borrow_mut().push(message.to_string());
    let deps = ReapStaleDaemonVersionsDeps {
        platform: scenario.platform,
        is_alive: scenario.alive,
        attest: &attest,
        send_signal: &send_signal,
        wait_for_exit: &wait_for_exit,
        remove_dir: &remove_dir,
        read_owner: &read_owner,
        log: &log,
        term_grace_ms: 1,
        kill_grace_ms: 1,
    };
    reap_stale_daemon_versions(&fixture.own, &deps)
}

fn old_dir(fixture: &Fixture) -> PathBuf {
    let dir = fixture.base.join("v1.0.0");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn scenario<'a>(owner_value: Option<DaemonOwner>, alive: &'a dyn Fn(u32) -> bool) -> Scenario<'a> {
    Scenario {
        owner: owner_value,
        alive,
        attest: true,
        exits_after: &|signal| signal == ReapSignal::Term,
        platform: ReapPlatform::Darwin,
    }
}

#[test]
fn live_attested_older_daemon_is_terminated_and_dir_removed() {
    let fixture = fixture();
    let dir = old_dir(&fixture);
    let recorder = Recorder::default();
    let results = run(&fixture, &scenario(Some(owner(4242)), &|_| true), &recorder);
    assert_eq!(
        results,
        vec![VersionReapResult {
            version: "1.0.0".to_string(),
            status: VersionReapStatus::Terminated,
            reason: "terminated attested older daemon pid 4242 with SIGTERM".to_string(),
        }]
    );
    assert_eq!(*recorder.signals.borrow(), vec![(4242, ReapSignal::Term)]);
    assert_eq!(*recorder.removed.borrow(), vec![dir]);
}

#[test]
fn own_version_dir_is_never_touched() {
    let fixture = fixture();
    let recorder = Recorder::default();
    let results = run(&fixture, &scenario(Some(owner(4242)), &|_| true), &recorder);
    assert!(results.is_empty());
    assert!(recorder.signals.borrow().is_empty());
    assert!(recorder.removed.borrow().is_empty());
    assert!(fixture.own.dir.exists());
}

#[test]
fn failed_attestation_spares_and_logs() {
    let fixture = fixture();
    old_dir(&fixture);
    let recorder = Recorder::default();
    let mut scenario = scenario(Some(owner(77)), &|_| true);
    scenario.attest = false;
    let results = run(&fixture, &scenario, &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Spared);
    assert_eq!(
        results[0].reason,
        "pid 77 attestation failed; possible recycled pid"
    );
    assert!(recorder.signals.borrow().is_empty());
    assert!(recorder.removed.borrow().is_empty());
    assert_eq!(
        recorder.logs.borrow()[0],
        "reap: sparing v1.0.0: pid 77 is alive but cmdline attestation failed (possible recycled pid)"
    );
}

#[test]
fn win32_defers_with_logged_reason() {
    let fixture = fixture();
    old_dir(&fixture);
    let recorder = Recorder::default();
    let mut scenario = scenario(Some(owner(77)), &|_| true);
    scenario.platform = ReapPlatform::Win32;
    let results = run(&fixture, &scenario, &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Deferred);
    assert_eq!(
        results[0].reason,
        "Windows cannot prove pid ownership safely; named-pipe reap deferred"
    );
    assert!(recorder.logs.borrow()[0].contains("named-pipe policy"));
    assert!(recorder.signals.borrow().is_empty());
}

#[test]
fn dead_owner_dir_is_removed_without_signaling() {
    let fixture = fixture();
    let dir = old_dir(&fixture);
    let recorder = Recorder::default();
    let results = run(&fixture, &scenario(Some(owner(55)), &|_| false), &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Removed);
    assert_eq!(
        results[0].reason,
        "removed stale version dir for dead owner pid 55"
    );
    assert!(recorder.signals.borrow().is_empty());
    assert_eq!(*recorder.removed.borrow(), vec![dir]);
}

#[test]
fn missing_owner_metadata_dir_is_removed() {
    let fixture = fixture();
    let dir = old_dir(&fixture);
    let recorder = Recorder::default();
    let results = run(&fixture, &scenario(None, &|_| false), &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Removed);
    assert_eq!(
        results[0].reason,
        "removed stale version dir without readable owner metadata"
    );
    assert_eq!(*recorder.removed.borrow(), vec![dir]);
}

#[test]
fn missing_owner_but_live_lock_holder_is_spared() {
    let fixture = fixture();
    let dir = old_dir(&fixture);
    std::fs::write(dir.join("daemon.lock"), "31337").unwrap();
    let recorder = Recorder::default();
    let results = run(&fixture, &scenario(None, &|pid| pid == 31337), &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Spared);
    assert_eq!(
        results[0].reason,
        "owner metadata missing but lock held by live pid 31337"
    );
    assert!(recorder.removed.borrow().is_empty());
}

#[test]
fn survivor_of_sigterm_is_escalated_to_sigkill() {
    let fixture = fixture();
    old_dir(&fixture);
    let recorder = Recorder::default();
    let mut scenario = scenario(Some(owner(9)), &|_| true);
    scenario.exits_after = &|signal| signal == ReapSignal::Kill;
    let results = run(&fixture, &scenario, &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Terminated);
    assert_eq!(
        results[0].reason,
        "terminated attested older daemon pid 9 after SIGKILL escalation"
    );
    assert_eq!(
        *recorder.signals.borrow(),
        vec![(9, ReapSignal::Term), (9, ReapSignal::Kill)]
    );
    assert_eq!(recorder.removed.borrow().len(), 1);
}

#[test]
fn survivor_of_sigkill_is_deferred_and_dir_kept() {
    let fixture = fixture();
    old_dir(&fixture);
    let recorder = Recorder::default();
    let mut scenario = scenario(Some(owner(9)), &|_| true);
    scenario.exits_after = &|_| false;
    let results = run(&fixture, &scenario, &recorder);
    assert_eq!(results[0].status, VersionReapStatus::Deferred);
    assert_eq!(
        results[0].reason,
        "attested daemon pid 9 survived SIGKILL; dir kept"
    );
    assert!(recorder.removed.borrow().is_empty());
    assert_eq!(
        recorder.logs.borrow().last().unwrap(),
        "reap: deferring v1.0.0: pid 9 survived SIGKILL"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_version_dir_is_not_followed() {
    let fixture = fixture();
    let target = fixture._root.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, fixture.base.join("v1.0.0")).unwrap();
    let recorder = Recorder::default();
    let results = run(&fixture, &scenario(None, &|_| false), &recorder);
    assert!(results.is_empty());
    assert!(recorder.removed.borrow().is_empty());
    assert!(target.exists());
}

#[test]
fn invalid_entry_names_are_ignored() {
    let fixture = fixture();
    for name in ["v", "v.bad", "notaversion", "v-1"] {
        std::fs::create_dir_all(fixture.base.join(name)).unwrap();
    }
    let recorder = Recorder::default();
    assert!(run(&fixture, &scenario(None, &|_| false), &recorder).is_empty());
}

fn attest_with(proc_file: Option<&[u8]>, ps: Option<&str>, platform: ReapPlatform) -> bool {
    let proc_bytes = proc_file.map(<[u8]>::to_vec);
    let ps_line = ps.map(str::to_string);
    let read_proc_file = move |_path: &str| proc_bytes.clone();
    let execute_for_stdout = move |file: &str, args: &[String]| {
        assert_eq!(file, "/bin/ps");
        assert_eq!(args, ["-p", "42", "-o", "command="]);
        ps_line.clone()
    };
    let deps = DaemonCliAttestationDeps {
        read_proc_file: &read_proc_file,
        execute_for_stdout: &execute_for_stdout,
    };
    attest_daemon_cli_process(42, platform, &deps)
}

#[test]
fn linux_node_cli_daemon_cmdline_is_proven() {
    assert!(attest_with(
        Some(b"/usr/bin/node\0/opt/omo/dist/cli.js\0daemon\0"),
        None,
        ReapPlatform::Linux
    ));
    assert!(attest_with(
        Some(b"/opt/omo/bin/lsp-daemon\0daemon\0"),
        None,
        ReapPlatform::Linux
    ));
}

#[test]
fn linux_unrelated_cmdline_is_rejected() {
    assert!(!attest_with(
        Some(b"/usr/bin/python3\0cli.js\0daemon\0"),
        None,
        ReapPlatform::Linux
    ));
    assert!(!attest_with(
        Some(b"/usr/bin/node\0/opt/omo/dist/cli.js\0mcp\0"),
        None,
        ReapPlatform::Linux
    ));
}

#[test]
fn darwin_node_cli_daemon_ps_line_is_proven() {
    assert!(attest_with(
        None,
        Some("/opt/homebrew/bin/node /opt/omo/dist/cli.js daemon\n"),
        ReapPlatform::Darwin
    ));
    assert!(attest_with(
        None,
        Some("/opt/omo/bin/lsp-daemon daemon\n"),
        ReapPlatform::Darwin
    ));
}

#[test]
fn darwin_unrelated_ps_line_is_rejected() {
    assert!(!attest_with(
        None,
        Some("/usr/bin/vim notes-daemon.txt\n"),
        ReapPlatform::Darwin
    ));
    assert!(!attest_with(
        None,
        Some("/usr/bin/nodemon cli.jsx daemon"),
        ReapPlatform::Darwin
    ));
}

#[test]
fn unreadable_process_table_is_rejected() {
    assert!(!attest_with(None, None, ReapPlatform::Darwin));
    assert!(!attest_with(None, None, ReapPlatform::Linux));
}

#[test]
fn win32_cannot_prove_ownership() {
    assert!(!attest_with(
        Some(b"node\0cli.js\0daemon\0"),
        Some("node cli.js daemon"),
        ReapPlatform::Win32
    ));
}
