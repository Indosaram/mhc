//! Ports of process-sweep-families, lsp-daemon-owner-attestation,
//! codegraph-process-sweep, codegraph-worker-process-sweep,
//! codegraph-worker-throttle and codegraph-zombie-sweep TS tests.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use pretty_assertions::assert_eq;
use serde_json::json;
use utils::process_sweep::*;

struct RecordingKiller {
    alive: bool,
    calls: RefCell<Vec<String>>,
    record_alive: bool,
}

impl RecordingKiller {
    fn new(alive: bool) -> Self {
        Self {
            alive,
            calls: RefCell::new(Vec::new()),
            record_alive: false,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl ProcessKiller for RecordingKiller {
    fn is_alive(&self, pid: u32) -> bool {
        if self.record_alive {
            self.calls.borrow_mut().push(format!("alive:{pid}"));
        }
        self.alive
    }

    fn kill(&self, pid: u32) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("kill:{pid}"));
        Ok(())
    }

    fn terminate(&self, pid: u32) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("term:{pid}"));
        Ok(())
    }
}

fn proc(command: impl Into<String>, pid: u32, ppid: u32) -> ProcessInfo {
    ProcessInfo {
        command: command.into(),
        pid,
        ppid,
    }
}

fn roots(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn select_codegraph(
    table: &[ProcessInfo],
    owned: &[&str],
    platform: &str,
) -> Vec<CodegraphZombieProcess> {
    let owned_roots = roots(owned);
    select_zombie_codegraph_processes(
        table,
        &SelectZombieCodegraphProcessesOptions {
            owned_roots: &owned_roots,
            platform: Some(platform),
        },
    )
}

fn select_proxies(
    table: &[ProcessInfo],
    owned: &[&str],
    platform: &str,
) -> Vec<LspDaemonProxyProcess> {
    let owned_roots = roots(owned);
    select_orphaned_lsp_daemon_proxies(
        table,
        &SelectOrphanedLspDaemonProxiesOptions {
            owned_roots: &owned_roots,
            platform: Some(platform),
        },
    )
}

fn pids<T: SweepTarget>(items: &[T]) -> Vec<u32> {
    items.iter().map(SweepTarget::pid).collect()
}

fn kinds(items: &[CodegraphZombieProcess]) -> Vec<(u32, &'static str)> {
    items
        .iter()
        .map(|item| (item.pid, item.match_kind.as_str()))
        .collect()
}

const NODE: &str = "/usr/local/bin/node";

// ---- process-sweep-families.test.ts: family matrix ----

#[test]
fn posix_table_each_family_selects_exactly_its_own_zombies() {
    let omo_root = "/tmp/omo-plugin";
    let install_dir = "/tmp/omo-install";
    let table = vec![
        proc("codex app-server", 200, 1),
        proc(
            format!("node {omo_root}/components/codegraph/dist/serve.js"),
            301,
            1,
        ),
        proc(
            format!(
                "node {omo_root}/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            302,
            9999,
        ),
        proc(
            format!("{install_dir}/bin/codegraph serve --mcp --path /tmp/proj"),
            303,
            1,
        ),
        proc(
            format!("node {omo_root}/components/lsp-daemon/dist/cli.js mcp"),
            304,
            1,
        ),
        proc(
            format!("node {omo_root}/components/lsp-daemon/dist/cli.js mcp"),
            305,
            200,
        ),
        proc(
            format!("node {omo_root}/components/lsp-daemon/dist/cli.js daemon"),
            306,
            1,
        ),
        proc(
            format!("bun {omo_root}/components/lsp-daemon/src/cli.ts mcp"),
            307,
            9999,
        ),
        proc(
            "node /tmp/other-plugin/components/lsp-daemon/dist/cli.js mcp",
            308,
            1,
        ),
    ];

    let codegraph = select_codegraph(&table, &[omo_root, install_dir], "linux");
    let proxies = select_proxies(&table, &[omo_root], "linux");

    assert_eq!(
        kinds(&codegraph),
        vec![
            (301, "serve-wrapper"),
            (302, "upstream-codegraph"),
            (303, "upstream-daemon")
        ]
    );
    assert_eq!(
        proxies
            .iter()
            .map(|item| (item.pid, item.match_kind.as_str()))
            .collect::<Vec<_>>(),
        vec![(304, "lsp-daemon-proxy"), (307, "lsp-daemon-proxy")]
    );
}

#[test]
fn windows_table_shapes_are_selected_per_family() {
    let win_root = r"C:\Users\runner\.codex\plugins\cache\sisyphuslabs\omo\4.15.1";
    let table = vec![
        proc("codex.exe app-server", 200, 4),
        proc(
            format!(r"node {win_root}\components\codegraph\dist\serve.js"),
            311,
            1,
        ),
        proc(
            format!(r"node.exe {win_root}\components\lsp-daemon\dist\cli.js mcp"),
            312,
            1,
        ),
        proc(
            format!(r"node.exe {win_root}\components\lsp-daemon\dist\cli.js mcp"),
            313,
            200,
        ),
        proc(
            format!(r"node.exe {win_root}\components\lsp-daemon\dist\cli.js daemon"),
            314,
            1,
        ),
    ];

    assert_eq!(
        pids(&select_codegraph(&table, &[win_root], "win32")),
        vec![311]
    );
    assert_eq!(
        pids(&select_proxies(&table, &[win_root], "win32")),
        vec![312]
    );
}

#[test]
fn daemon_token_alongside_mcp_is_never_a_proxy() {
    let omo_root = "/tmp/omo-plugin";
    let table = vec![
        proc(
            format!("node {omo_root}/components/lsp-daemon/dist/cli.js mcp daemon"),
            320,
            1,
        ),
        proc(
            format!("node {omo_root}/components/lsp-daemon/dist/cli.js mcp"),
            321,
            1,
        ),
    ];
    assert_eq!(
        pids(&select_proxies(&table, &[omo_root], "linux")),
        vec![321]
    );
}

#[test]
fn mcp_token_of_unrelated_cli_is_ignored() {
    let omo_root = "/tmp/omo-plugin";
    let table = vec![
        proc(
            format!("node {omo_root}/components/other-tool/dist/cli.js mcp"),
            330,
            1,
        ),
        proc(
            format!("/usr/bin/python3 {omo_root}/components/lsp-daemon/dist/cli.js mcp"),
            331,
            1,
        ),
    ];
    assert_eq!(select_proxies(&table, &[omo_root], "linux"), Vec::new());
}

// ---- process-sweep-families.test.ts: proxy sweep ----

#[test]
fn proxy_sweep_kills_only_orphan_and_writes_family_stamp() {
    let home = tempfile::tempdir().expect("tempdir");
    let omo_root = "/tmp/omo-plugin";
    let killer = RecordingKiller::new(false);
    let provider = || {
        Ok(vec![
            proc("codex app-server", 200, 1),
            proc(
                format!("node {omo_root}/components/lsp-daemon/dist/cli.js mcp"),
                401,
                1,
            ),
            proc(
                format!("node {omo_root}/components/lsp-daemon/dist/cli.js mcp"),
                402,
                200,
            ),
        ])
    };
    let result = sweep_orphaned_lsp_daemon_proxies(&SweepOrphanedLspDaemonProxiesOptions {
        base_dir: LspDaemonBaseDirOptions {
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        },
        sweep: ProcessFamilySweepOptions {
            force: true,
            grace_ms: Some(0),
            killer: Some(&killer),
            platform: Some("linux"),
            ..Default::default()
        },
        owned_roots: Some(roots(&[omo_root])),
        process_provider: Some(&provider),
        ..Default::default()
    });

    assert_eq!(result.result.action, ProcessSweepAction::Swept);
    assert_eq!(pids(&result.result.killed), vec![401]);
    assert_eq!(killer.calls(), vec!["term:401"]);
    assert_eq!(
        result.result.stamp_file,
        home.path().join(".maho/lsp-daemon/lsp-proxy-sweep.stamp")
    );
    assert!(result.result.stamp_file.exists());
    assert!(
        !home
            .path()
            .join(".maho/codegraph/zombie-sweep.stamp")
            .exists()
    );
}

#[test]
fn proxy_family_is_throttled_independently() {
    let home = tempfile::tempdir().expect("tempdir");
    let now_ms = 1_784_602_800_000;
    let base_dir = LspDaemonBaseDirOptions {
        home_dir: Some(home.path().to_path_buf()),
        ..Default::default()
    };
    let empty = || Ok(Vec::new());
    sweep_orphaned_lsp_daemon_proxies(&SweepOrphanedLspDaemonProxiesOptions {
        base_dir: base_dir.clone(),
        sweep: ProcessFamilySweepOptions {
            force: true,
            now_ms: Some(now_ms),
            ..Default::default()
        },
        owned_roots: Some(roots(&["/tmp/omo-plugin"])),
        process_provider: Some(&empty),
        ..Default::default()
    });

    let failing = || -> Result<Vec<ProcessInfo>, String> {
        panic!("process provider should not run while throttled")
    };
    let result = sweep_orphaned_lsp_daemon_proxies(&SweepOrphanedLspDaemonProxiesOptions {
        base_dir,
        sweep: ProcessFamilySweepOptions {
            now_ms: Some(now_ms + 1_000),
            ..Default::default()
        },
        owned_roots: Some(roots(&["/tmp/omo-plugin"])),
        process_provider: Some(&failing),
        ..Default::default()
    });

    assert_eq!(result.result.action, ProcessSweepAction::Throttled);
}

// ---- process-sweep-families.test.ts: stale lsp-daemon versions ----

fn write_owner(version_dir: &Path, pid: u32) {
    fs::create_dir_all(version_dir).expect("mkdir");
    let body = json!({
        "endpoint": { "kind": "unix", "path": "/tmp/daemon.sock", "dev": 1, "ino": 1 },
        "nonce": "nonce",
        "pid": pid,
        "startedAt": "2026-07-21T00:00:00.000Z",
    });
    fs::write(version_dir.join("daemon.owner"), format!("{body}\n")).expect("write owner");
}

fn home_base(home: &Path) -> (LspDaemonBaseDirOptions, PathBuf) {
    (
        LspDaemonBaseDirOptions {
            home_dir: Some(home.to_path_buf()),
            ..Default::default()
        },
        home.join(".maho/lsp-daemon"),
    )
}

#[test]
fn stale_attested_daemon_is_killed_and_current_is_left_alone() {
    let home = tempfile::tempdir().expect("tempdir");
    let (base, base_dir) = home_base(home.path());
    write_owner(&base_dir.join("v0.0.1"), 710);
    write_owner(&base_dir.join("v9.9.9"), 711);
    let killer = RecordingKiller::new(false);
    let attest = |pid: u32, _: &str| pid == 710 || pid == 711;
    let alive = |_: u32| true;

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: base,
        sweep: ProcessFamilySweepOptions {
            force: true,
            grace_ms: Some(0),
            killer: Some(&killer),
            platform: Some("linux"),
            ..Default::default()
        },
        attest: Some(&attest),
        current_version: Some("9.9.9".to_string()),
        is_alive: Some(&alive),
        ..Default::default()
    });

    assert_eq!(result.action, LspDaemonVersionSweepAction::Swept);
    assert_eq!(
        result
            .killed
            .iter()
            .map(|target| (target.target.pid, target.version.as_str()))
            .collect::<Vec<_>>(),
        vec![(710, "0.0.1")]
    );
    assert_eq!(pids(&result.candidates), vec![710]);
    assert_eq!(killer.calls(), vec!["term:710"]);
    assert_eq!(result.stamp_file, base_dir.join("lsp-daemon-sweep.stamp"));
    assert!(result.stamp_file.exists());
}

#[test]
fn stale_version_failing_attestation_is_spared_as_recycled_pid() {
    let home = tempfile::tempdir().expect("tempdir");
    let (base, base_dir) = home_base(home.path());
    write_owner(&base_dir.join("v0.0.1"), 720);
    let killer = RecordingKiller::new(true);
    let logs = RefCell::new(Vec::<String>::new());
    let log = |message: &str| logs.borrow_mut().push(message.to_string());
    let attest = |_: u32, _: &str| false;
    let alive = |_: u32| true;

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: base,
        sweep: ProcessFamilySweepOptions {
            force: true,
            killer: Some(&killer),
            log: Some(&log),
            platform: Some("linux"),
            ..Default::default()
        },
        attest: Some(&attest),
        current_version: Some("9.9.9".to_string()),
        is_alive: Some(&alive),
        ..Default::default()
    });

    assert_eq!(result.killed, Vec::new());
    assert_eq!(
        result
            .spared
            .iter()
            .map(|spared| spared.target.target.pid)
            .collect::<Vec<_>>(),
        vec![720]
    );
    assert!(logs.borrow().iter().any(|message| message.contains("720")));
}

#[test]
fn stale_version_on_windows_is_spared() {
    let home = tempfile::tempdir().expect("tempdir");
    let (base, base_dir) = home_base(home.path());
    write_owner(&base_dir.join("v0.0.1"), 730);
    let alive = |_: u32| true;

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: base,
        sweep: ProcessFamilySweepOptions {
            force: true,
            platform: Some("win32"),
            ..Default::default()
        },
        current_version: Some("9.9.9".to_string()),
        is_alive: Some(&alive),
        ..Default::default()
    });

    assert_eq!(result.killed, Vec::new());
    assert_eq!(
        result
            .spared
            .iter()
            .map(|spared| (spared.target.target.pid, spared.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![(730, "windows-attestation-unsupported")]
    );
}

#[test]
fn stale_version_with_dead_owner_has_nothing_to_kill() {
    let home = tempfile::tempdir().expect("tempdir");
    let (base, base_dir) = home_base(home.path());
    write_owner(&base_dir.join("v0.0.1"), 740);
    let dead = |_: u32| false;

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: base,
        sweep: ProcessFamilySweepOptions {
            force: true,
            ..Default::default()
        },
        current_version: Some("9.9.9".to_string()),
        is_alive: Some(&dead),
        ..Default::default()
    });

    assert_eq!(
        (result.candidates, result.killed, result.spared),
        (Vec::new(), Vec::new(), Vec::new())
    );
}

#[test]
fn unknown_current_version_skips_conservatively() {
    let home = tempfile::tempdir().expect("tempdir");
    let base_dir = home.path().join(".maho/lsp-daemon");
    write_owner(&base_dir.join("v0.0.1"), 750);

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: LspDaemonBaseDirOptions {
            env: Some(Default::default()),
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        },
        sweep: ProcessFamilySweepOptions {
            force: true,
            ..Default::default()
        },
        ..Default::default()
    });

    assert_eq!(result.action, LspDaemonVersionSweepAction::Skipped);
    assert_eq!(result.killed, Vec::new());
}

#[test]
fn non_version_entries_in_base_dir_are_ignored() {
    let home = tempfile::tempdir().expect("tempdir");
    let (base, base_dir) = home_base(home.path());
    fs::create_dir_all(base_dir.join("not-a-version")).expect("mkdir");
    fs::write(base_dir.join("vfile"), "not a dir").expect("write");
    write_owner(&base_dir.join("v0.0.1"), 760);
    let attest = |_: u32, _: &str| true;
    let alive = |_: u32| true;

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: base,
        sweep: ProcessFamilySweepOptions {
            dry_run: true,
            force: true,
            ..Default::default()
        },
        attest: Some(&attest),
        current_version: Some("9.9.9".to_string()),
        is_alive: Some(&alive),
        ..Default::default()
    });

    assert_eq!(
        result
            .candidates
            .iter()
            .map(|target| target.version.as_str())
            .collect::<Vec<_>>(),
        vec!["0.0.1"]
    );
    assert_eq!(result.killed, Vec::new());
}

#[test]
fn identity_change_after_term_never_escalates_to_kill() {
    let home = tempfile::tempdir().expect("tempdir");
    let (base, base_dir) = home_base(home.path());
    write_owner(&base_dir.join("v1.0.0"), 4242);
    let killer = RecordingKiller::new(true);
    let attest_calls = RefCell::new(0);
    let attest_target = |_: &StaleLspDaemonVersionTarget, _: &str| {
        *attest_calls.borrow_mut() += 1;
        *attest_calls.borrow() < 3
    };
    let alive = |_: u32| true;

    let result = sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions {
        base_dir: base,
        sweep: ProcessFamilySweepOptions {
            force: true,
            grace_ms: Some(0),
            killer: Some(&killer),
            platform: Some("linux"),
            ..Default::default()
        },
        attest_target: Some(&attest_target),
        current_version: Some("2.0.0".to_string()),
        is_alive: Some(&alive),
        ..Default::default()
    });

    assert_eq!(*attest_calls.borrow(), 3);
    assert_eq!(killer.calls(), vec!["term:4242"]);
    assert_eq!(result.killed, Vec::new());
}

// ---- process-sweep-families.test.ts: cli attestation ----

fn proc_file(bytes: &'static [u8]) -> impl Fn(&str) -> Result<Vec<u8>, String> {
    move |_| Ok(bytes.to_vec())
}

#[test]
fn linux_cmdline_node_cli_daemon_attests() {
    let read = proc_file(b"node\0/usr/lib/omo/lsp-daemon/dist/cli.js\0daemon\0");
    assert!(attest_lsp_daemon_cli_process(
        801,
        "linux",
        &LspDaemonAttestationDeps {
            read_proc_file: Some(&read),
            ..Default::default()
        }
    ));
}

#[test]
fn linux_cmdline_mcp_proxy_shape_fails() {
    let read = proc_file(b"node\0/usr/lib/omo/lsp-daemon/dist/cli.js\0mcp\0");
    assert!(!attest_lsp_daemon_cli_process(
        802,
        "linux",
        &LspDaemonAttestationDeps {
            read_proc_file: Some(&read),
            ..Default::default()
        }
    ));
}

#[test]
fn unreadable_linux_cmdline_fails_closed() {
    let read = |_: &str| -> Result<Vec<u8>, String> { Err("ENOENT".to_string()) };
    assert!(!attest_lsp_daemon_cli_process(
        803,
        "linux",
        &LspDaemonAttestationDeps {
            read_proc_file: Some(&read),
            ..Default::default()
        }
    ));
}

#[test]
fn macos_ps_command_line_attestation() {
    let daemon =
        |_: &str, _: &[&str]| Some("node /usr/lib/omo/lsp-daemon/dist/cli.js daemon\n".to_string());
    let mcp =
        |_: &str, _: &[&str]| Some("node /usr/lib/omo/lsp-daemon/dist/cli.js mcp\n".to_string());
    let none = |_: &str, _: &[&str]| None;
    let deps = |execute: LspDaemonExecuteForStdout<'static>| LspDaemonAttestationDeps {
        execute_for_stdout: Some(execute),
        ..Default::default()
    };
    let daemon: &'static _ = Box::leak(Box::new(daemon));
    let mcp: &'static _ = Box::leak(Box::new(mcp));
    let none: &'static _ = Box::leak(Box::new(none));
    assert_eq!(
        [
            attest_lsp_daemon_cli_process(804, "darwin", &deps(daemon)),
            attest_lsp_daemon_cli_process(805, "darwin", &deps(mcp)),
            attest_lsp_daemon_cli_process(806, "darwin", &deps(none)),
        ],
        [true, false, false]
    );
}

#[test]
fn windows_cli_attestation_always_fails_closed() {
    assert!(!attest_lsp_daemon_cli_process(
        807,
        "win32",
        &LspDaemonAttestationDeps::default()
    ));
}

// "backward-compatible codegraph/process-sweep import path": N/A, JS module
// re-export identity; the Rust surface is the single `utils::process_sweep` path.

// ---- lsp-daemon-owner-attestation.test.ts ----

fn owner_fixture() -> LspDaemonOwnerIdentity {
    LspDaemonOwnerIdentity {
        endpoint: LspDaemonOwnerEndpoint {
            dev: 1,
            ino: 2,
            path: "/tmp/daemon.sock".to_string(),
        },
        nonce: "owner-nonce".to_string(),
        pid: 4242,
        started_at: "2026-07-30T00:00:00.000Z".to_string(),
    }
}

fn owner_target(owner: &LspDaemonOwnerIdentity) -> LspDaemonOwnerTarget {
    LspDaemonOwnerTarget {
        auth_path: PathBuf::from("/tmp/auth.token"),
        owner: owner.clone(),
        owner_path: PathBuf::from("/tmp/daemon.owner"),
        pid: owner.pid,
    }
}

#[test]
fn generic_daemon_argv_without_owner_ping_is_spared() {
    let owner = owner_fixture();
    let target = owner_target(&owner);
    let read_proc = proc_file(b"/usr/bin/node\0/tmp/cli.js\0daemon\0");
    let generic = attest_lsp_daemon_cli_process(
        owner.pid,
        "linux",
        &LspDaemonAttestationDeps {
            read_proc_file: Some(&read_proc),
            ..Default::default()
        },
    );
    let owner_json = owner.to_json().to_string();
    let read_text = |path: &Path| {
        Ok(if path == target.owner_path {
            owner_json.clone()
        } else {
            "auth-token".to_string()
        })
    };
    let ping = |_: &LspDaemonOwnerEndpoint, _: &str| None;
    let attested = attest_lsp_daemon_owner(
        &target,
        &LspDaemonOwnerAttestationDeps {
            ping_owner: Some(&ping),
            read_text: Some(&read_text),
        },
    );

    assert_eq!((generic, attested), (true, false));
}

#[test]
fn unchanged_owner_with_authenticated_ping_is_proven() {
    let owner = owner_fixture();
    let target = owner_target(&owner);
    let owner_json = owner.to_json().to_string();
    let read_text = |path: &Path| {
        Ok(if path == target.owner_path {
            owner_json.clone()
        } else {
            "auth-token".to_string()
        })
    };
    let ping =
        |_: &LspDaemonOwnerEndpoint, token: &str| (token == "auth-token").then(owner_fixture);
    assert!(attest_lsp_daemon_owner(
        &target,
        &LspDaemonOwnerAttestationDeps {
            ping_owner: Some(&ping),
            read_text: Some(&read_text)
        }
    ));
}

#[test]
fn owner_ping_request_matches_daemon_wire_protocol() {
    assert_eq!(
        create_lsp_daemon_owner_ping_request("auth-token"),
        json!({
            "id": "process-sweep-owner-attestation",
            "jsonrpc": "2.0",
            "method": "omo/ping",
            "params": { "_omo": { "protocolVersion": 1, "token": "auth-token" } },
        })
    );
}

#[test]
fn owner_target_points_at_daemon_auth_token_file() {
    let version_dir = tempfile::tempdir().expect("tempdir");
    let owner = LspDaemonOwnerIdentity {
        endpoint: LspDaemonOwnerEndpoint {
            dev: 1,
            ino: 2,
            path: version_dir
                .path()
                .join("daemon.sock")
                .to_string_lossy()
                .into_owned(),
        },
        ..owner_fixture()
    };
    fs::write(
        version_dir.path().join("daemon.owner"),
        owner.to_json().to_string(),
    )
    .expect("write owner");

    assert_eq!(
        read_lsp_daemon_owner_target(version_dir.path()),
        Some(LspDaemonOwnerTarget {
            auth_path: version_dir.path().join("daemon.auth"),
            owner: owner.clone(),
            owner_path: version_dir.path().join("daemon.owner"),
            pid: owner.pid,
        })
    );
}

#[cfg(unix)]
#[test]
fn owner_ping_round_trips_over_a_unix_socket() {
    // A unix-socket peer stands in for the lsp-daemon request router.
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let owner = LspDaemonOwnerIdentity {
        endpoint: LspDaemonOwnerEndpoint {
            dev: 1,
            ino: 2,
            path: socket.to_string_lossy().into_owned(),
        },
        ..owner_fixture()
    };
    let owner_json = owner.to_json();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).expect("read");
        let request: serde_json::Value = serde_json::from_str(&line).expect("json");
        let mut result = owner_json;
        result["protocolVersion"] = json!(1);
        let authorized = request["params"]["_omo"]["token"] == "router-token";
        let response = if authorized {
            json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
        } else {
            json!({ "jsonrpc": "2.0", "id": request["id"], "error": { "code": -32001 } })
        };
        (&stream)
            .write_all(format!("{response}\n").as_bytes())
            .expect("write");
    });
    fs::write(dir.path().join("daemon.owner"), owner.to_json().to_string()).expect("owner");
    fs::write(dir.path().join("daemon.auth"), "router-token\n").expect("auth");
    let target = read_lsp_daemon_owner_target(dir.path()).expect("target");

    assert!(attest_lsp_daemon_owner(
        &target,
        &LspDaemonOwnerAttestationDeps::default()
    ));
    server.join().expect("server");
}

// ---- codegraph-process-sweep.test.ts ----

#[test]
fn orphaned_owned_codegraph_commands_ppid_one_and_dead_parent() {
    let omo_root = "/tmp/omo-owned-plugin";
    let table = vec![
        proc("codex app-server", 200, 1),
        proc(
            format!("{NODE} {omo_root}/components/codegraph/dist/serve.js"),
            301,
            1,
        ),
        proc(
            format!(
                "{NODE} {omo_root}/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            302,
            9999,
        ),
        proc(
            format!(
                "{NODE} {omo_root}/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            303,
            200,
        ),
        proc(
            format!(
                "{NODE} /tmp/not-omo/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            304,
            1,
        ),
    ];
    assert_eq!(
        pids(&select_codegraph(&table, &[omo_root], "linux")),
        vec![301, 302]
    );
}

#[test]
fn windows_owned_root_uses_platform_resolution() {
    let omo_root = r"C:\Users\runner\.codex\plugins\cache\sisyphuslabs\omo\4.15.1";
    let table = vec![proc(
        format!(r"{NODE} {omo_root}\components\codegraph\dist\serve.js"),
        305,
        1,
    )];
    assert_eq!(
        pids(&select_codegraph(&table, &[omo_root], "win32")),
        vec![305]
    );
}

#[test]
fn sibling_path_sharing_root_prefix_is_ignored() {
    let version_root = "/tmp/codex/plugins/cache/sisyphuslabs/omo/4.15.1";
    let table = vec![
        proc(
            format!(
                "{NODE} /tmp/omo-evil/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            311,
            1,
        ),
        proc(
            format!(
                "{NODE} /tmp/codex/plugins/cache/sisyphuslabs/omo/4.15.10/components/codegraph/dist/serve.js"
            ),
            312,
            1,
        ),
        proc(
            format!("{NODE} {version_root}/components/codegraph/dist/serve.js"),
            313,
            1,
        ),
    ];
    assert_eq!(
        pids(&select_codegraph(
            &table,
            &["/tmp/omo", version_root],
            "linux"
        )),
        vec![313]
    );
}

#[test]
fn owned_root_in_a_different_argument_is_ignored() {
    let table = vec![
        proc(
            format!(
                "{NODE} /opt/not-omo/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp --cache /tmp/omo"
            ),
            321,
            1,
        ),
        proc(
            format!(
                "{NODE} /tmp/omo/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            322,
            1,
        ),
    ];
    assert_eq!(
        pids(&select_codegraph(&table, &["/tmp/omo"], "linux")),
        vec![322]
    );
}

#[test]
fn upstream_package_path_as_data_argument_is_ignored() {
    let table = vec![
        proc(
            "/usr/bin/python3 /tmp/tool.py --template /tmp/omo/node_modules/@colbymchenry/codegraph/README.md",
            323,
            1,
        ),
        proc(
            format!(
                "{NODE} /tmp/omo/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
            ),
            324,
            1,
        ),
    ];
    assert_eq!(
        pids(&select_codegraph(&table, &["/tmp/omo"], "linux")),
        vec![324]
    );
}

#[test]
fn serve_wrapper_mentioned_only_as_data_is_ignored() {
    let wrapper = "/tmp/omo/components/codegraph/dist/serve.js";
    let table = vec![
        proc(
            format!("/usr/bin/python3 /tmp/tool.py --template {wrapper}"),
            331,
            1,
        ),
        proc(format!("{NODE} {wrapper}.backup"), 332, 1),
        proc(format!("{NODE} {wrapper}"), 333, 1),
    ];
    assert_eq!(
        pids(&select_codegraph(&table, &["/tmp/omo"], "linux")),
        vec![333]
    );
}

#[test]
fn detached_upstream_daemon_extracts_project_root() {
    let command = format!(
        "{NODE} /tmp/omo/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp --path /tmp/proj-a"
    );
    assert_eq!(
        select_codegraph(&[proc(command.clone(), 341, 1)], &["/tmp/omo"], "linux"),
        vec![CodegraphZombieProcess {
            command,
            daemon_project_root: Some("/tmp/proj-a".to_string()),
            match_kind: CodegraphProcessMatchKind::UpstreamDaemon,
            matched_root: "/tmp/omo".to_string(),
            pid: 341,
            ppid: 1,
        }]
    );
}

fn daemon_shapes(items: &[CodegraphZombieProcess]) -> Vec<(u32, &'static str, Option<String>)> {
    items
        .iter()
        .map(|item| {
            (
                item.pid,
                item.match_kind.as_str(),
                item.daemon_project_root.clone(),
            )
        })
        .collect()
}

#[test]
fn provisioned_launcher_and_bundle_are_daemon_shaped() {
    let dir = "/tmp/omo-install";
    let table = vec![
        proc(
            format!("{dir}/bin/codegraph serve --mcp --path /tmp/proj-b"),
            351,
            1,
        ),
        proc(
            format!(
                "{dir}/node --liftoff-only {dir}/lib/dist/bin/codegraph.js serve --mcp --path /tmp/proj-b"
            ),
            352,
            1,
        ),
    ];
    let root = Some("/tmp/proj-b".to_string());
    assert_eq!(
        daemon_shapes(&select_codegraph(&table, &[dir], "linux")),
        vec![
            (351, "upstream-daemon", root.clone()),
            (352, "upstream-daemon", root)
        ]
    );
}

#[test]
fn windows_standalone_daemon_preserves_raw_path() {
    let dir = r"C:\Users\runner\.omo\codegraph";
    let table = vec![
        proc(
            format!(r"{dir}\bin\codegraph.exe serve --mcp --path C:\proj\app"),
            353,
            1,
        ),
        proc(
            format!(
                r"C:\node\node.exe {dir}\lib\dist\bin\codegraph.js serve --mcp --path C:\proj\app"
            ),
            354,
            1,
        ),
    ];
    let root = Some(r"C:\proj\app".to_string());
    assert_eq!(
        daemon_shapes(&select_codegraph(&table, &[dir], "win32")),
        vec![
            (353, "upstream-daemon", root.clone()),
            (354, "upstream-daemon", root)
        ]
    );
}

#[test]
fn upstream_serve_without_path_stays_plain_upstream() {
    let base = format!(
        "{NODE} /tmp/omo/node_modules/@colbymchenry/codegraph/bin/codegraph.js serve --mcp"
    );
    let table = vec![
        proc(base.clone(), 355, 1),
        proc(format!("{base} --path"), 356, 1),
    ];
    assert_eq!(
        kinds(&select_codegraph(&table, &["/tmp/omo"], "linux")),
        vec![(355, "upstream-codegraph"), (356, "upstream-codegraph")]
    );
}

#[test]
fn daemon_with_live_parent_is_not_a_candidate() {
    let table = vec![
        proc("codex app-server", 200, 1),
        proc(
            "/tmp/omo-install/bin/codegraph serve --mcp --path /tmp/proj-c",
            357,
            200,
        ),
    ];
    assert_eq!(
        select_codegraph(&table, &["/tmp/omo-install"], "linux"),
        Vec::new()
    );
}

#[test]
fn daemon_outside_owned_roots_is_ignored() {
    let table = vec![
        proc(
            "/opt/not-omo/bin/codegraph serve --mcp --path /tmp/proj-d",
            358,
            1,
        ),
        proc(
            "/opt/not-omo/node /opt/not-omo/lib/dist/bin/codegraph.js serve --mcp --path /tmp/proj-d",
            359,
            1,
        ),
    ];
    assert_eq!(select_codegraph(&table, &["/tmp/omo"], "linux"), Vec::new());
}

#[test]
fn quoted_path_with_spaces_is_extracted() {
    let table = vec![proc(
        r#"/tmp/omo-install/bin/codegraph serve --mcp --path "/tmp/proj with spaces""#,
        360,
        1,
    )];
    assert_eq!(
        select_codegraph(&table, &["/tmp/omo-install"], "linux")
            .into_iter()
            .map(|item| item.daemon_project_root)
            .collect::<Vec<_>>(),
        vec![Some("/tmp/proj with spaces".to_string())]
    );
}

#[test]
fn posix_ps_table_preserves_pid_ppid_and_command() {
    let output = [
        "  101     1 /usr/bin/node /tmp/omo/components/codegraph/dist/serve.js",
        "  202   101 /bin/sh -lc echo still includes spaces",
        "not-a-pid line",
    ]
    .join("\n");
    assert_eq!(
        parse_posix_process_table(&output),
        vec![
            proc(
                "/usr/bin/node /tmp/omo/components/codegraph/dist/serve.js",
                101,
                1
            ),
            proc("/bin/sh -lc echo still includes spaces", 202, 101),
        ]
    );
}

#[test]
fn windows_process_table_parses_array_and_single_object() {
    let array = r#"[{"ProcessId":10,"ParentProcessId":4,"CommandLine":"node a.js"},{"ProcessId":11,"ParentProcessId":4,"CommandLine":"  "}]"#;
    let single = r#"{"ProcessId":12,"ParentProcessId":1,"CommandLine":"node b.js"}"#;
    assert_eq!(
        (
            parse_windows_process_table(array),
            parse_windows_process_table(single),
            parse_windows_process_table("not json")
        ),
        (
            vec![proc("node a.js", 10, 4)],
            vec![proc("node b.js", 12, 1)],
            Vec::new()
        )
    );
}

#[test]
fn only_sisyphuslabs_plugin_cache_is_trusted() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let trusted = codex_home
        .path()
        .join("plugins/cache/sisyphuslabs/omo/4.15.1");
    let untrusted = codex_home.path().join("plugins/cache/evil/omo/1.0.0");
    fs::create_dir_all(&trusted).expect("mkdir");
    fs::create_dir_all(&untrusted).expect("mkdir");
    let discovered = discover_codegraph_owned_roots(&CodegraphOwnedRootsOptions {
        codex_home: Some(codex_home.path().to_path_buf()),
        home_dir: Some(codex_home.path().join("home")),
        ..Default::default()
    });
    let has = |path: &Path| {
        discovered.iter().any(|root| {
            Path::new(root) == path
                || fs::canonicalize(path).is_ok_and(|real| Path::new(root) == real)
        })
    };
    assert_eq!((has(&trusted), has(&untrusted)), (true, false));
}

#[test]
fn ambient_codegraph_install_dir_is_not_trusted() {
    let home = tempfile::tempdir().expect("tempdir");
    let discovered = discover_codegraph_owned_roots(&CodegraphOwnedRootsOptions {
        env: Some(
            [(
                "CODEGRAPH_INSTALL_DIR".to_string(),
                "/opt/not-omo".to_string(),
            )]
            .into(),
        ),
        home_dir: Some(home.path().to_path_buf()),
        ..Default::default()
    });
    let expected = home
        .path()
        .join(".maho/codegraph")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        (
            discovered.contains(&"/opt/not-omo".to_string()),
            discovered.contains(&expected)
        ),
        (false, true)
    );
}

// ---- codegraph-worker-process-sweep.test.ts ----

#[test]
fn quoted_real_world_posix_workers_are_matched() {
    let claude = "/Users/yeongyu/.claude/omo";
    let provisioned = "/Users/yeongyu/.omo/codegraph";
    let platform_pkg = format!("{claude}/node_modules/@colbymchenry/codegraph-darwin-arm64");
    let table = vec![
        proc(
            format!(
                "{platform_pkg}/node --liftoff-only {platform_pkg}/lib/dist/bin/codegraph.js status --json"
            ),
            92635,
            1,
        ),
        proc(
            format!(
                "{provisioned}/node --liftoff-only {provisioned}/lib/dist/bin/codegraph.js status --json"
            ),
            93383,
            1,
        ),
    ];
    assert_eq!(
        kinds(&select_codegraph(&table, &[claude, provisioned], "darwin")),
        vec![(92635, "upstream-codegraph"), (93383, "upstream-codegraph")]
    );
}

#[test]
fn orphaned_npm_shim_selects_whole_worker_family() {
    let root = "/Users/yeongyu/.claude/omo";
    let platform_pkg = format!("{root}/node_modules/@colbymchenry/codegraph-darwin-arm64");
    let table = vec![
        proc(
            format!("node {root}/node_modules/@colbymchenry/codegraph/npm-shim.js status --json"),
            94100,
            1,
        ),
        proc(
            format!(
                "{platform_pkg}/node --liftoff-only {platform_pkg}/lib/dist/bin/codegraph.js status --json"
            ),
            94101,
            94100,
        ),
    ];
    assert_eq!(
        pids(&select_codegraph(&table, &[root], "darwin")),
        vec![94100, 94101]
    );
}

#[test]
fn live_worker_family_is_not_condemned() {
    let root = "/Users/yeongyu/.claude/omo";
    let platform_pkg = format!("{root}/node_modules/@colbymchenry/codegraph-darwin-arm64");
    let table = vec![
        proc(
            format!("node {root}/components/codegraph/dist/cli.js hook session-start-worker"),
            94200,
            200,
        ),
        proc(
            format!("node {root}/node_modules/@colbymchenry/codegraph/npm-shim.js sync"),
            94201,
            94200,
        ),
        proc(
            format!(
                "{platform_pkg}/node --liftoff-only {platform_pkg}/lib/dist/bin/codegraph.js sync"
            ),
            94202,
            94201,
        ),
        proc("codex app-server", 200, 1),
    ];
    assert_eq!(select_codegraph(&table, &[root], "darwin"), Vec::new());
}

#[test]
fn windows_platform_and_provisioned_workers_are_matched() {
    let plugin = r"C:\Users\runner\.codex\plugins\cache\sisyphuslabs\omo\4.19.2";
    let provisioned = r"C:\Users\runner\.omo\codegraph";
    let platform_pkg = format!(r"{plugin}\node_modules\@colbymchenry\codegraph-win32-x64");
    let table = vec![
        proc(
            format!(
                r"{platform_pkg}\node.exe --liftoff-only {platform_pkg}\lib\dist\bin\codegraph.js status --json"
            ),
            95100,
            1,
        ),
        proc(
            format!(
                r"{provisioned}\node.exe --liftoff-only {provisioned}\lib\dist\bin\codegraph.js index"
            ),
            95101,
            1,
        ),
    ];
    assert_eq!(
        pids(&select_codegraph(&table, &[plugin, provisioned], "win32")),
        vec![95100, 95101]
    );
}

#[test]
fn default_home_roots_include_claude_omo_install() {
    let home = PathBuf::from("/Users/yeongyu");
    let discovered = discover_codegraph_owned_roots(&CodegraphOwnedRootsOptions {
        codex_home: Some(home.join(".missing-codex")),
        home_dir: Some(home.clone()),
        ..Default::default()
    });
    assert!(discovered.contains(&home.join(".claude/omo").to_string_lossy().into_owned()));
}

#[test]
fn detached_provisioned_daemon_classification_is_intact() {
    let root = "/Users/yeongyu/.omo/codegraph";
    let command = format!(
        "{root}/node --liftoff-only {root}/lib/dist/bin/codegraph.js serve --mcp --path /tmp/live-project"
    );
    assert_eq!(
        daemon_shapes(&select_codegraph(
            &[proc(command, 95200, 1)],
            &[root],
            "darwin"
        )),
        vec![(
            95200,
            "upstream-daemon",
            Some("/tmp/live-project".to_string())
        )]
    );
}

// ---- codegraph-worker-throttle.test.ts / codegraph-zombie-sweep.test.ts ----

fn sweep_options<'a>(
    home: Option<&Path>,
    owned: &[&str],
    provider: ProcessProvider<'a>,
    sweep: ProcessFamilySweepOptions<'a>,
) -> SweepCodegraphZombiesOptions<'a> {
    SweepCodegraphZombiesOptions {
        roots: CodegraphOwnedRootsOptions {
            home_dir: home.map(Path::to_path_buf),
            ..Default::default()
        },
        sweep,
        owned_roots: Some(roots(owned)),
        process_provider: Some(provider),
    }
}

fn write_daemon_lock(project: &Path, body: &str) {
    fs::create_dir_all(project.join(".codegraph")).expect("mkdir");
    fs::write(project.join(".codegraph/daemon.pid"), body).expect("write lock");
}

fn daemon_lock_body(pid: u32, version: &str) -> String {
    let body = json!({ "pid": pid, "socketPath": "/tmp/daemon.sock", "startedAt": 1_784_615_252_733_i64, "version": version });
    format!("{}\n", serde_json::to_string_pretty(&body).expect("json"))
}

const PINNED: &str = utils::codegraph::CODEGRAPH_PINNED_VERSION;

#[test]
fn fresh_daemon_stamp_still_reaps_orphaned_workers() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = "/tmp/omo-codegraph-worker-throttle";
    let now_ms = 1_784_854_800_000;
    let empty = || Ok(Vec::new());
    sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &[dir],
        &empty,
        ProcessFamilySweepOptions {
            force: true,
            now_ms: Some(now_ms),
            ..Default::default()
        },
    ));
    let provider = || {
        Ok(vec![
            proc(
                format!("{dir}/node --liftoff-only {dir}/lib/dist/bin/codegraph.js status --json"),
                96100,
                1,
            ),
            proc(
                format!(
                    "{dir}/node --liftoff-only {dir}/lib/dist/bin/codegraph.js serve --mcp --path /tmp/project"
                ),
                96101,
                1,
            ),
        ])
    };
    let killer = RecordingKiller::new(false);

    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &[dir],
        &provider,
        ProcessFamilySweepOptions {
            grace_ms: Some(0),
            killer: Some(&killer),
            now_ms: Some(now_ms + 1_000),
            platform: Some("darwin"),
            ..Default::default()
        },
    ));

    assert_eq!(
        (result.action, result.worker_action, result.daemon_action),
        (
            ProcessSweepAction::Swept,
            ProcessSweepAction::Swept,
            ProcessSweepAction::Throttled
        )
    );
    assert_eq!(pids(&result.killed), vec![96100]);
    assert_eq!(killer.calls(), vec!["term:96100"]);
    assert_ne!(result.worker_stamp_file, result.daemon_stamp_file);
    assert_eq!(result.stamp_file, result.daemon_stamp_file);
}

#[test]
fn live_daemon_lock_spares_daemon_while_worker_dies() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = "/tmp/omo-codegraph-worker-daemon";
    let project = home.path().join("project");
    write_daemon_lock(&project, "96201\n");
    let project_str = project.to_string_lossy().into_owned();
    let provider = || {
        Ok(vec![
            proc(
                format!("{dir}/node --liftoff-only {dir}/lib/dist/bin/codegraph.js sync"),
                96200,
                1,
            ),
            proc(
                format!(
                    "{dir}/node --liftoff-only {dir}/lib/dist/bin/codegraph.js serve --mcp --path {project_str}"
                ),
                96201,
                1,
            ),
        ])
    };
    let killer = RecordingKiller::new(false);

    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &[dir],
        &provider,
        ProcessFamilySweepOptions {
            force: true,
            grace_ms: Some(0),
            killer: Some(&killer),
            platform: Some("darwin"),
            ..Default::default()
        },
    ));

    assert_eq!(
        (pids(&result.killed), pids(&result.spared)),
        (vec![96200], vec![96201])
    );
    assert_eq!(killer.calls(), vec!["term:96200"]);
}

#[test]
fn dry_run_reports_candidates_without_killing() {
    let home = tempfile::tempdir().expect("tempdir");
    let omo_root = "/tmp/omo-owned-plugin";
    let provider = || {
        Ok(vec![proc(
            format!("{NODE} {omo_root}/components/codegraph/dist/serve.js"),
            401,
            1,
        )])
    };
    let killer = RecordingKiller::new(true);

    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &[omo_root],
        &provider,
        ProcessFamilySweepOptions {
            dry_run: true,
            force: true,
            killer: Some(&killer),
            platform: Some("linux"),
            ..Default::default()
        },
    ));

    assert_eq!(
        (pids(&result.killed), pids(&result.candidates)),
        (Vec::new(), vec![401])
    );
    assert_eq!(killer.calls(), Vec::<String>::new());
}

#[test]
fn fresh_daemon_stamp_without_force_checks_workers_once() {
    let home = tempfile::tempdir().expect("tempdir");
    let now_ms = 1_783_299_600_000;
    let empty = || Ok(Vec::new());
    let first = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &["/tmp/omo"],
        &empty,
        ProcessFamilySweepOptions {
            force: true,
            now_ms: Some(now_ms),
            ..Default::default()
        },
    ));
    let stamp_time = UNIX_EPOCH
        + Duration::from_millis(u64::try_from(now_ms - 30 * 60 * 1_000).expect("positive"));
    fs::File::options()
        .write(true)
        .open(&first.daemon_stamp_file)
        .and_then(|file| file.set_modified(stamp_time))
        .expect("utimes");
    let calls = RefCell::new(0);
    let counting = || {
        *calls.borrow_mut() += 1;
        Ok(Vec::new())
    };

    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &["/tmp/omo"],
        &counting,
        ProcessFamilySweepOptions {
            now_ms: Some(now_ms),
            ..Default::default()
        },
    ));

    assert_eq!(
        (
            result.action,
            result.worker_action,
            result.daemon_action,
            *calls.borrow()
        ),
        (
            ProcessSweepAction::Swept,
            ProcessSweepAction::Swept,
            ProcessSweepAction::Throttled,
            1
        )
    );
}

#[test]
fn force_bypasses_throttle_and_refreshes_stamp() {
    let home = tempfile::tempdir().expect("tempdir");
    let now_ms = 1_783_303_200_000;
    let empty = || Ok(Vec::new());
    let first = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &["/tmp/omo"],
        &empty,
        ProcessFamilySweepOptions {
            force: true,
            now_ms: Some(now_ms - 10 * 60 * 1_000),
            ..Default::default()
        },
    ));

    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &["/tmp/omo"],
        &empty,
        ProcessFamilySweepOptions {
            force: true,
            now_ms: Some(now_ms),
            ..Default::default()
        },
    ));

    let mtime = fs::metadata(&first.stamp_file)
        .and_then(|meta| meta.modified())
        .expect("mtime");
    let mtime_ms =
        i64::try_from(mtime.duration_since(UNIX_EPOCH).expect("epoch").as_millis()).expect("fits");
    assert_eq!(result.action, ProcessSweepAction::Swept);
    assert!(mtime_ms >= now_ms, "{mtime_ms} < {now_ms}");
}

#[test]
fn zombie_surviving_term_is_escalated_to_kill() {
    let home = tempfile::tempdir().expect("tempdir");
    let omo_root = "/tmp/omo-owned-plugin";
    let provider = || {
        Ok(vec![proc(
            format!("{NODE} {omo_root}/components/codegraph/dist/serve.js"),
            501,
            1,
        )])
    };
    let killer = RecordingKiller {
        record_alive: true,
        ..RecordingKiller::new(true)
    };

    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &[omo_root],
        &provider,
        ProcessFamilySweepOptions {
            force: true,
            grace_ms: Some(0),
            killer: Some(&killer),
            platform: Some("linux"),
            ..Default::default()
        },
    ));

    assert_eq!(pids(&result.killed), vec![501]);
    assert_eq!(killer.calls(), vec!["term:501", "alive:501", "kill:501"]);
}

struct DaemonCase {
    lock: Option<String>,
    nested: bool,
    dry_run: bool,
}

struct DaemonOutcome {
    candidates: Vec<u32>,
    killed: Vec<u32>,
    spared: Vec<u32>,
    calls: Vec<String>,
    logs: Vec<String>,
}

fn run_daemon_case(pid: u32, launcher: bool, case: DaemonCase) -> DaemonOutcome {
    let home = tempfile::tempdir().expect("tempdir");
    let project = home.path().join("proj");
    let path_arg = if case.nested {
        project.join("sub/dir")
    } else {
        project.clone()
    };
    fs::create_dir_all(&path_arg).expect("mkdir");
    if let Some(body) = &case.lock {
        write_daemon_lock(&project, body);
    }
    let dir = "/tmp/omo-install";
    let path_str = path_arg.to_string_lossy().into_owned();
    let command = if launcher {
        format!("{dir}/bin/codegraph serve --mcp --path {path_str}")
    } else {
        format!(
            "{dir}/node --liftoff-only {dir}/lib/dist/bin/codegraph.js serve --mcp --path {path_str}"
        )
    };
    let provider = || Ok(vec![proc(command.clone(), pid, 1)]);
    let killer = RecordingKiller::new(false);
    let logs = RefCell::new(Vec::<String>::new());
    let log = |message: &str| logs.borrow_mut().push(message.to_string());
    let result = sweep_codegraph_zombies(&sweep_options(
        Some(home.path()),
        &[dir],
        &provider,
        ProcessFamilySweepOptions {
            dry_run: case.dry_run,
            force: true,
            grace_ms: Some(0),
            killer: Some(&killer),
            log: Some(&log),
            platform: Some("linux"),
            ..Default::default()
        },
    ));
    if let Some(first) = result.candidates.first() {
        assert_eq!(first.match_kind, CodegraphProcessMatchKind::UpstreamDaemon);
    }
    DaemonOutcome {
        candidates: pids(&result.candidates),
        killed: pids(&result.killed),
        spared: pids(&result.spared),
        calls: killer.calls(),
        logs: logs.into_inner(),
    }
}

#[test]
fn live_daemon_with_matching_lock_is_spared_and_logged() {
    let outcome = run_daemon_case(
        601,
        false,
        DaemonCase {
            lock: Some(daemon_lock_body(601, PINNED)),
            nested: false,
            dry_run: false,
        },
    );
    assert_eq!(
        (
            outcome.candidates,
            outcome.spared,
            outcome.killed,
            outcome.calls
        ),
        (vec![601], vec![601], vec![], vec![])
    );
    assert!(outcome.logs.iter().any(|message| message.contains("601")));
}

#[test]
fn daemon_without_lockfile_is_killed_as_stale() {
    let outcome = run_daemon_case(
        602,
        true,
        DaemonCase {
            lock: None,
            nested: false,
            dry_run: false,
        },
    );
    assert_eq!(
        (outcome.candidates, outcome.killed, outcome.spared),
        (vec![602], vec![602], vec![])
    );
}

#[test]
fn daemon_with_mismatched_lock_pid_is_killed_as_stale() {
    let outcome = run_daemon_case(
        603,
        true,
        DaemonCase {
            lock: Some(daemon_lock_body(999, PINNED)),
            nested: false,
            dry_run: false,
        },
    );
    assert_eq!((outcome.killed, outcome.spared), (vec![603], vec![]));
}

#[test]
fn daemon_with_unparseable_lock_is_spared_and_logged() {
    let outcome = run_daemon_case(
        604,
        true,
        DaemonCase {
            lock: Some("not-a-pid-not-json\n".to_string()),
            nested: false,
            dry_run: false,
        },
    );
    assert_eq!((outcome.killed, outcome.spared), (vec![], vec![604]));
    assert!(outcome.logs.iter().any(|message| message.contains("604")));
}

#[test]
fn legacy_plain_pid_lock_spares_daemon() {
    let outcome = run_daemon_case(
        605,
        true,
        DaemonCase {
            lock: Some("605\n".to_string()),
            nested: false,
            dry_run: false,
        },
    );
    assert_eq!((outcome.killed, outcome.spared), (vec![], vec![605]));
}

#[test]
fn older_version_daemon_with_matching_lock_is_spared() {
    let outcome = run_daemon_case(
        606,
        true,
        DaemonCase {
            lock: Some(daemon_lock_body(606, "1.0.1")),
            nested: false,
            dry_run: false,
        },
    );
    assert_eq!((outcome.killed, outcome.spared), (vec![], vec![606]));
}

#[test]
fn lock_at_initialized_ancestor_spares_daemon() {
    let outcome = run_daemon_case(
        607,
        true,
        DaemonCase {
            lock: Some(daemon_lock_body(607, PINNED)),
            nested: true,
            dry_run: false,
        },
    );
    assert_eq!((outcome.killed, outcome.spared), (vec![], vec![607]));
}

#[test]
fn dry_run_against_live_daemon_lists_it_as_spared() {
    let outcome = run_daemon_case(
        608,
        true,
        DaemonCase {
            lock: Some(daemon_lock_body(608, PINNED)),
            nested: false,
            dry_run: true,
        },
    );
    assert_eq!(
        (
            outcome.candidates,
            outcome.spared,
            outcome.killed,
            outcome.calls
        ),
        (vec![608], vec![608], vec![], vec![])
    );
}
