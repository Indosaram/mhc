//! Port of `packages/coding-agent/test/kill-guard/{judge-fixtures,judge-patterns,judge-signals}.test.ts`.
//! Deterministic unit/integration tests on synthetic process tables; nothing sends a real signal.

use maho_core::kill_guard::{
    KILL_GUARD_TAG, KillGuardContext, KillGuardRule, KillGuardShell, KillGuardVantage,
    KillGuardVerdict, PatternMode, ProcessRow, ProcessView, SelectResult, SignalCall, SignalId,
    judge_argv, judge_command, judge_pattern, judge_signal,
};
use std::sync::{Arc, Mutex};

const UID: u32 = 501;
const AGENT: i64 = 500;

fn row(
    pid: i64,
    ppid: i64,
    pgid: i64,
    sid: i64,
    comm: &str,
    args: Option<&str>,
    uid: Option<u32>,
) -> ProcessRow {
    ProcessRow {
        pid,
        ppid,
        pgid,
        sid,
        uid: uid.unwrap_or(UID),
        comm: comm.to_owned(),
        args: args.unwrap_or(comm).to_owned(),
    }
}

fn synthetic_table() -> Vec<ProcessRow> {
    vec![
        row(1, 0, 1, 1, "launchd", Some("/sbin/launchd"), Some(0)),
        row(
            200,
            1,
            200,
            200,
            "systemd",
            Some("/usr/lib/systemd/systemd --user"),
            None,
        ),
        row(300, 1, 300, 300, "tmux", Some("tmux new -s work"), None),
        row(400, 300, 400, 400, "zsh", Some("-zsh"), None),
        row(AGENT, 400, AGENT, 400, "bun", Some("bun senpi"), None),
        row(
            600,
            300,
            600,
            600,
            "node",
            Some("node vite --port 5173"),
            None,
        ),
        row(
            700,
            1,
            700,
            700,
            "redis-server",
            Some("redis-server *:6379"),
            None,
        ),
        row(800, AGENT, 800, 800, "sleep", Some("sleep 30"), None),
        row(
            900,
            AGENT,
            900,
            900,
            "python3",
            Some("python3 prelude.py"),
            None,
        ),
        row(901, 900, 900, 900, "sleep", Some("sleep 60"), None),
        row(4000, 300, 4000, 4000, "worker", Some("omo-worker-1"), None),
        row(4001, 4000, 4000, 4000, "worker", Some("omo-worker-2"), None),
    ]
}

type TestSelectFn = Arc<dyn Fn(&[String]) -> Option<Vec<i64>> + Send + Sync>;

#[derive(Default)]
struct FixtureOpts {
    platform: Option<&'static str>,
    vantage: Option<KillGuardVantage>,
    shell: Option<KillGuardShell>,
    unreadable_table: bool,
    select_fn: Option<TestSelectFn>,
}

struct Fixture {
    context: KillGuardContext,
    selections: Arc<Mutex<Vec<Vec<String>>>>,
}

fn fixture(opts: FixtureOpts) -> Fixture {
    let selections: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let selections_clone = Arc::clone(&selections);
    let table = if opts.unreadable_table {
        None
    } else {
        Some(synthetic_table())
    };
    let select_fn = opts.select_fn;

    let context = KillGuardContext {
        platform: opts.platform.unwrap_or("linux").to_owned(),
        agent_pid: AGENT,
        agent_ppid: Some(400),
        vantage: opts.vantage.unwrap_or(KillGuardVantage::FreshSession),
        shell: opts.shell.unwrap_or(KillGuardShell::Bash),
        view: Arc::new(move || {
            let rows = table.clone()?;
            let selections_inner = Arc::clone(&selections_clone);
            let select_fn_inner = select_fn.clone();
            Some(ProcessView {
                rows,
                select: Arc::new(move |args| {
                    selections_inner
                        .lock()
                        .expect("selections lock")
                        .push(args.to_vec());
                    match &select_fn_inner {
                        Some(func) => match func(args) {
                            Some(pids) => SelectResult::Ok { pids },
                            None => SelectResult::Err {
                                reason: "pgrep exited 2".into(),
                            },
                        },
                        None => SelectResult::Ok { pids: Vec::new() },
                    }
                }),
            })
        }),
    };

    Fixture {
        context,
        selections,
    }
}

fn fixture_with_pids(pids: Vec<i64>) -> Fixture {
    fixture(FixtureOpts {
        select_fn: Some(Arc::new(move |_| Some(pids.clone()))),
        ..Default::default()
    })
}

fn rule_tag(verdict: &KillGuardVerdict) -> &'static str {
    match verdict {
        KillGuardVerdict::Allow => "allow",
        KillGuardVerdict::Defer { .. } => "defer",
        KillGuardVerdict::Refuse { rule, .. } => match rule {
            KillGuardRule::R1 => "R1",
            KillGuardRule::R2 => "R2",
            KillGuardRule::R3 => "R3",
            KillGuardRule::R4 => "R4",
            KillGuardRule::R5 => "R5",
        },
    }
}

#[test]
fn broad_pattern_ere_reader_refuses_broad_and_allows_narrow() {
    let broad_cases = [
        (r#"paid" 2>/dev/null || echo 1"#, PatternMode::Substring),
        ("foo|", PatternMode::Substring),
        ("|bar", PatternMode::Substring),
        ("a||b", PatternMode::Substring),
        ("(|x)", PatternMode::Substring),
        ("(x|)", PatternMode::Substring),
        ("()", PatternMode::Substring),
        ("", PatternMode::Substring),
        (".*", PatternMode::Substring),
        ("^", PatternMode::Substring),
        ("^$", PatternMode::Substring),
        ("x*", PatternMode::Substring),
        ("(ab)?", PatternMode::Substring),
        ("[a-z]*", PatternMode::Substring),
        ("[a-z]", PatternMode::Substring),
        ("[^x][^y]", PatternMode::Substring),
        (".+.", PatternMode::Substring),
        (r"\w\s", PatternMode::Substring),
        ("a.*", PatternMode::Substring),
        ("a.", PatternMode::Substring),
        ("^a.*", PatternMode::Substring),
        ("vite|a", PatternMode::Substring),
        ("R", PatternMode::Substring),
        ("^x$", PatternMode::Substring),
        ("(unbalanced", PatternMode::Substring),
        ("[unbalanced", PatternMode::Substring),
        ("a{2", PatternMode::Substring),
        ("ab{3,1}", PatternMode::Substring),
        (".*", PatternMode::Exact),
        ("", PatternMode::Exact),
    ];
    for (pattern, mode) in broad_cases {
        assert!(
            judge_pattern(pattern, mode).is_broad(),
            "expected {pattern:?} ({mode:?}) to be broad"
        );
    }

    let narrow_cases = [
        ("vite --port 5173", PatternMode::Substring),
        ("^node .*/server.js$", PatternMode::Substring),
        ("ab", PatternMode::Substring),
        ("vite|ab", PatternMode::Substring),
        ("my-worker-[0-9]+", PatternMode::Substring),
        ("omo-worker-[0-9]+", PatternMode::Substring),
        (r"\bvite\b", PatternMode::Substring),
        ("a{2}", PatternMode::Substring),
        ("R", PatternMode::Exact),
        ("X", PatternMode::Exact),
        ("redis-server", PatternMode::Exact),
        ("g++", PatternMode::Exact),
    ];
    for (pattern, mode) in narrow_cases {
        assert!(
            !judge_pattern(pattern, mode).is_broad(),
            "expected {pattern:?} ({mode:?}) to be narrow"
        );
    }
}

#[test]
fn pkill_killall_and_kill_command_lines() {
    // Reporter's command refused before any selection run (C18)
    let f = fixture(FixtureOpts::default());
    let verdict = judge_command(
        r#"pkill -f 'paid" 2>/dev/null || echo 1' ; true"#,
        &f.context,
    );
    assert_eq!(rule_tag(&verdict), "R1");
    assert!(f.selections.lock().expect("lock").is_empty());
    let KillGuardVerdict::Refuse { message, .. } = verdict else {
        panic!("expected refusal");
    };
    assert!(message.starts_with(&format!("{KILL_GUARD_TAG} ")));
    assert!(!message.contains('\n'));
    assert!(message.contains("terminal.killGuard"));

    // One-letter substring rewrite suggestion (C8s)
    let verdict = judge_command("pkill R", &fixture(FixtureOpts::default()).context);
    assert_eq!(rule_tag(&verdict), "R1");
    if let KillGuardVerdict::Refuse { message, .. } = verdict {
        assert!(message.contains("pkill -x R"));
    }

    // R1 broad patterns (A2)
    for cmd in [
        "pkill -f 'a.*'",
        "pkill -f 'a.'",
        "pkill -f '^a.*'",
        "pkill -f 'vite|a'",
        "pkill -x '.*'",
        r#"pkill -f """#,
        "pkill -9 -f '.*'",
        "pkill --signal KILL -f '|x'",
        "sudo pkill -f '.*'",
        "timeout 5 pkill -f 'x*'",
        r#"bash -c "pkill -f '.*'""#,
        "pgrep -f 'a||b' | xargs kill",
    ] {
        assert_eq!(
            rule_tag(&judge_command(
                cmd,
                &fixture(FixtureOpts::default()).context
            )),
            "R1",
            "expected R1 for {cmd}"
        );
    }

    // Narrow commands selecting non-protected process (C7, C8, C8x, C9)
    for cmd in [
        "pkill -f 'vite --port 5173'",
        "pkill -f '^node .*/server.js$'",
        "pkill -f ab",
        "pkill -f 'vite|ab'",
        "pkill -x R",
        "pkill -x X",
        "pkill -x redis-server",
        "pkill -P 4242",
        "pkill -0 -f vite",
    ] {
        assert_eq!(
            judge_command(cmd, &fixture_with_pids(vec![600]).context),
            KillGuardVerdict::Allow,
            "expected Allow for {cmd}"
        );
    }

    // Drops pkill-only options from dry run (A3)
    let f = fixture_with_pids(vec![600]);
    assert_eq!(
        judge_command("pkill -e -f 'vite --port 5173'", &f.context),
        KillGuardVerdict::Allow
    );
    assert_eq!(
        judge_command("pkill --queue 5 -f 'vite --port 5173'", &f.context),
        KillGuardVerdict::Allow
    );
    assert_eq!(
        *f.selections.lock().expect("lock"),
        vec![
            vec!["-f", "vite --port 5173"],
            vec!["-f", "vite --port 5173"],
        ]
    );

    // Drops BSD -I from dry run (A3)
    let f = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![600]))),
        ..Default::default()
    });
    assert_eq!(
        judge_command("pkill -I -f 'vite --port 5173'", &f.context),
        KillGuardVerdict::Allow
    );
    assert_eq!(
        *f.selections.lock().expect("lock"),
        vec![vec!["-a", "-f", "vite --port 5173"]]
    );

    // Unknown options -> R5
    assert_eq!(
        rule_tag(&judge_command(
            "pkill --frobnicate -f vite",
            &fixture(FixtureOpts::default()).context
        )),
        "R5"
    );
    assert_eq!(
        rule_tag(&judge_command(
            "pkill -M core -f vite",
            &fixture(FixtureOpts {
                platform: Some("darwin"),
                ..Default::default()
            })
            .context
        )),
        "R5"
    );

    // Protected selection -> R2 (C17, C21, C26, C9)
    let all_pids: Vec<i64> = synthetic_table().into_iter().map(|r| r.pid).collect();
    for (cmd, pids) in [
        ("pkill -u 501", vec![1, 200, 300, 400, AGENT, 600]),
        ("pkill -x bun", vec![AGENT]),
        ("pkill -f senpi", vec![AGENT]),
        ("pkill -v -f foo", all_pids),
        ("pkill -P 400", vec![AGENT]),
    ] {
        let verdict = judge_command(cmd, &fixture_with_pids(pids).context);
        assert_eq!(rule_tag(&verdict), "R2", "expected R2 for {cmd}");
        if let KillGuardVerdict::Refuse { message, .. } = verdict {
            assert!(message.contains(&format!("{AGENT} bun")));
        }
    }

    // User service manager protected on Linux
    let verdict = judge_command("pkill -x systemd", &fixture_with_pids(vec![200]).context);
    assert_eq!(rule_tag(&verdict), "R2");

    // Darwin default ancestor exclusion vs `-a`
    let darwin_ancestors = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![AGENT, 400]))),
        ..Default::default()
    });
    assert_eq!(
        judge_command("pkill -f senpi", &darwin_ancestors.context),
        KillGuardVerdict::Allow
    );
    let darwin_all = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![AGENT]))),
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("pkill -a -f senpi", &darwin_all.context)),
        "R2"
    );

    // -g 0 vantage resolution
    let fresh = fixture_with_pids(vec![AGENT]);
    assert_eq!(
        judge_command("pkill -g 0 sleep", &fresh.context),
        KillGuardVerdict::Allow
    );
    assert!(fresh.selections.lock().expect("lock").is_empty());

    let in_thread = fixture(FixtureOpts {
        vantage: Some(KillGuardVantage::Agent),
        shell: Some(KillGuardShell::Bun),
        select_fn: Some(Arc::new(|_| Some(vec![AGENT]))),
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("pkill -g 0 sleep", &in_thread.context)),
        "R2"
    );
    assert_eq!(
        *in_thread.selections.lock().expect("lock"),
        vec![vec!["-g".to_owned(), AGENT.to_string(), "sleep".to_owned()]]
    );

    // killall -u and bare killall / pkill
    assert_eq!(
        rule_tag(&judge_command(
            "killall -u 501",
            &fixture_with_pids(vec![AGENT]).context
        )),
        "R2"
    );
    assert_eq!(
        rule_tag(&judge_command(
            "killall",
            &fixture(FixtureOpts::default()).context
        )),
        "R3"
    );
    assert_eq!(
        rule_tag(&judge_command(
            "pkill",
            &fixture(FixtureOpts::default()).context
        )),
        "R3"
    );

    // Allowed killall forms (C8, C8x)
    for (cmd, platform) in [
        ("killall Finder", "darwin"),
        ("killall -q myapp", "linux"),
        ("killall R", "linux"),
        ("killall -9 redis-server", "linux"),
    ] {
        let f = fixture(FixtureOpts {
            platform: Some(platform),
            select_fn: Some(Arc::new(|_| Some(vec![700]))),
            ..Default::default()
        });
        assert_eq!(judge_command(cmd, &f.context), KillGuardVerdict::Allow);
    }

    // Literal escaping in killall dry run
    let f = fixture(FixtureOpts::default());
    assert_eq!(
        judge_command("killall g++", &f.context),
        KillGuardVerdict::Allow
    );
    assert_eq!(
        *f.selections.lock().expect("lock"),
        vec![vec!["-x", r"g\+\+"]]
    );

    // BSD killall -m (C27)
    let darwin_none = fixture(FixtureOpts {
        platform: Some("darwin"),
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("killall -m '.*'", &darwin_none.context)),
        "R1"
    );
    let darwin_700 = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![700]))),
        ..Default::default()
    });
    assert_eq!(
        judge_command("killall -m 'ba.*'", &darwin_700.context),
        KillGuardVerdict::Allow
    );
    let darwin_400 = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![400]))),
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("killall -m 'ba.*'", &darwin_400.context)),
        "R2"
    );
    let darwin_agent = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![AGENT]))),
        ..Default::default()
    });
    assert_eq!(
        judge_command("killall -s Finder", &darwin_agent.context),
        KillGuardVerdict::Allow
    );

    // psmisc killall -g (C28)
    assert_eq!(
        rule_tag(&judge_command(
            "killall -g zsh",
            &fixture_with_pids(vec![400]).context
        )),
        "R4"
    );
    assert_eq!(
        judge_command("killall -g worker", &fixture_with_pids(vec![4000]).context),
        KillGuardVerdict::Allow
    );

    // BSD multi-pattern pkill (C29)
    assert_eq!(
        rule_tag(&judge_command("pkill foo b", &darwin_none.context)),
        "R1"
    );
    let darwin_600 = fixture(FixtureOpts {
        platform: Some("darwin"),
        select_fn: Some(Arc::new(|_| Some(vec![600]))),
        ..Default::default()
    });
    assert_eq!(
        judge_command("pkill foo bar", &darwin_600.context),
        KillGuardVerdict::Allow
    );

    // Opaque wrappers (R3) & non-signal applets
    for cmd in [
        "busybox kill -9 -1",
        "busybox sh -c 'kill -9 -1'",
        "fish -c 'kill -9 -1'",
        "env -S 'kill -9 -1'",
        "env --split-string='kill -9 -1'",
        "env -S'kill -9 -1'",
    ] {
        assert_eq!(
            rule_tag(&judge_command(
                cmd,
                &fixture(FixtureOpts::default()).context
            )),
            "R3",
            "expected R3 for {cmd}"
        );
    }
    for cmd in ["busybox echo kill -9 -1", "busybox ls"] {
        assert_eq!(
            judge_command(cmd, &fixture(FixtureOpts::default()).context),
            KillGuardVerdict::Allow
        );
    }

    // Age filters & fail-closed (R5, C25)
    assert_eq!(
        rule_tag(&judge_command(
            "killall -o 5m node",
            &fixture(FixtureOpts::default()).context
        )),
        "R5"
    );
    let failed_dry_run = fixture(FixtureOpts {
        select_fn: Some(Arc::new(|_| None)),
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("pkill -f vite", &failed_dry_run.context)),
        "R5"
    );

    let blind = fixture(FixtureOpts {
        unreadable_table: true,
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("pkill -f vite", &blind.context)),
        "R5"
    );
    assert_eq!(
        rule_tag(&judge_command("kill -- -4000", &blind.context)),
        "R5"
    );
    assert_eq!(
        judge_command("kill 12345", &blind.context),
        KillGuardVerdict::Allow
    );
    assert_eq!(rule_tag(&judge_command("kill 400", &blind.context)), "R2");
    assert_eq!(rule_tag(&judge_command("kill 1", &blind.context)), "R2");
}

#[test]
fn kill_targets_dynamic_args_and_signals() {
    let f = fixture(FixtureOpts::default());

    for cmd in [
        "kill 12345",
        "kill -9 12345",
        "kill -s TERM 12345",
        "kill %1",
        "kill -0 400",
        "kill -s 0 -1",
        "kill -l",
        "kill -L",
        "kill -- -4000",
        "kill -TERM -4000",
        "sudo kill 4242",
        "trap 'kill 0' EXIT; sleep 1",
        "docker kill web",
        "git log --grep kill",
        r#"echo "kill -9 -1""#,
        "systemctl --user stop app",
        "pgrep -af node",
        "pgrep -f 'my-worker-[0-9]+' | xargs kill",
    ] {
        assert_eq!(
            judge_command(cmd, &f.context),
            KillGuardVerdict::Allow,
            "expected Allow for {cmd}"
        );
    }

    for cmd in [
        "kill -9 -1",
        "kill -- -1",
        "kill -1",
        "kill -s KILL -1",
        "kill -TERM 12 -1",
    ] {
        assert_eq!(
            rule_tag(&judge_command(cmd, &f.context)),
            "R3",
            "expected R3 for {cmd}"
        );
    }

    let agent_kill = format!("kill -9 {AGENT}");
    for cmd in [
        "kill 1",
        "kill 400",
        "kill 300",
        &agent_kill,
        "kill -- -400",
    ] {
        let tag = rule_tag(&judge_command(cmd, &f.context));
        assert!(tag == "R2" || tag == "R4", "got {tag} for {cmd}");
    }

    // kill 0 vantage
    assert_eq!(
        rule_tag(&judge_command(
            "kill 0",
            &fixture(FixtureOpts {
                vantage: Some(KillGuardVantage::Agent),
                shell: Some(KillGuardShell::Bun),
                ..Default::default()
            })
            .context
        )),
        "R4"
    );
    assert_eq!(
        judge_command(
            "kill 0",
            &fixture(FixtureOpts {
                vantage: Some(KillGuardVantage::Process { pid: 900 }),
                shell: Some(KillGuardShell::Sh),
                ..Default::default()
            })
            .context
        ),
        KillGuardVerdict::Allow
    );
    assert_eq!(judge_command("kill 0", &f.context), KillGuardVerdict::Allow);

    // Native Windows (C24)
    let windows = fixture(FixtureOpts {
        platform: Some("win32"),
        unreadable_table: true,
        ..Default::default()
    });
    assert_eq!(
        rule_tag(&judge_command("kill -9 -1", &windows.context)),
        "R3"
    );
    assert_eq!(
        judge_command("pkill -f '.*'", &windows.context),
        KillGuardVerdict::Allow
    );

    // Dynamic arguments: defer vs R5
    for cmd in [
        r#"pkill -f "$PATTERN""#,
        "kill $PID",
        "kill $!",
        "pkill -u $USER",
        "kill -TERM $PPID",
    ] {
        assert_eq!(
            rule_tag(&judge_command(cmd, &f.context)),
            "defer",
            "expected defer for {cmd}"
        );
    }

    for (cmd, expected) in [
        ("sudo kill $PID", "defer"),
        ("sudo kill -TERM $PID", "defer"),
        (r#"env -i pkill -f "$P""#, "R5"),
        ("sudo kill -- -$G", "R5"),
        ("sudo kill -$SIG $PID", "R5"),
        (r#"/usr/bin/pkill -f "$P""#, "R5"),
        ("/bin/kill $PID", "R5"),
        ("su - root -c 'pkill -f $P'", "R5"),
    ] {
        assert_eq!(
            rule_tag(&judge_command(cmd, &f.context)),
            expected,
            "unexpected verdict for {cmd}"
        );
    }

    assert_eq!(
        judge_command(
            "/usr/bin/pkill -f foo",
            &fixture_with_pids(vec![600]).context
        ),
        KillGuardVerdict::Allow
    );

    let sh = fixture(FixtureOpts {
        shell: Some(KillGuardShell::Sh),
        vantage: Some(KillGuardVantage::Process { pid: 900 }),
        ..Default::default()
    });
    assert_eq!(rule_tag(&judge_command("kill -- -$G", &sh.context)), "R5");
    assert_eq!(
        rule_tag(&judge_command("kill -TERM -$(cat pgid)", &sh.context)),
        "R5"
    );
    assert_eq!(rule_tag(&judge_command("kill $PID", &sh.context)), "defer");
    assert_eq!(
        rule_tag(&judge_command("sh -c 'kill -- -$G'", &f.context)),
        "R5"
    );

    // Unparsed / nested quotes (C30)
    assert_eq!(
        rule_tag(&judge_command(r#"sh -c "pkill -f '$P'""#, &f.context)),
        "defer"
    );
    assert_eq!(
        rule_tag(&judge_command("case x in x) kill -9 -1;; esac", &f.context)),
        "R3"
    );
    assert_eq!(
        rule_tag(&judge_command(
            "case x in x) pkill -f vite;; esac",
            &f.context
        )),
        "defer"
    );
    assert_eq!(
        judge_command("case x in x) ls;; esac", &f.context),
        KillGuardVerdict::Allow
    );

    // judge_argv (E5)
    let bun_ctx = fixture(FixtureOpts {
        shell: Some(KillGuardShell::Bun),
        ..Default::default()
    })
    .context;
    let to_vec = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        rule_tag(&judge_argv(&to_vec(&["pkill", "-f", "a||b"]), &bun_ctx)),
        "R1"
    );
    assert_eq!(
        rule_tag(&judge_argv(&to_vec(&["sh", "-c", "kill -9 -1"]), &bun_ctx)),
        "R3"
    );
    assert_eq!(
        rule_tag(&judge_argv(
            &to_vec(&["/usr/bin/env", "FOO=1", "pkill", "-f", ".*"]),
            &bun_ctx
        )),
        "R1"
    );
    assert_eq!(
        judge_argv(&to_vec(&["echo", "pkill -f .*"]), &bun_ctx),
        KillGuardVerdict::Allow
    );

    // judge_signal in agent thread (E6)
    let agent_ctx = fixture(FixtureOpts {
        vantage: Some(KillGuardVantage::Agent),
        ..Default::default()
    })
    .context;
    let signal_cases = [
        (-1, false, Some(SignalId::Name("SIGTERM".into())), "R3"),
        (0, false, Some(SignalId::Name("SIGTERM".into())), "R4"),
        (1, false, Some(SignalId::Number(9)), "R2"),
        (400, false, Some(SignalId::Name("SIGTERM".into())), "R2"),
        (400, true, Some(SignalId::Name("SIGTERM".into())), "R4"),
        (0, true, Some(SignalId::Name("SIGKILL".into())), "R4"),
        (800, false, Some(SignalId::Name("SIGTERM".into())), "allow"),
        (
            -4000,
            false,
            Some(SignalId::Name("SIGTERM".into())),
            "allow",
        ),
        (-1, false, Some(SignalId::Number(0)), "allow"),
        (1, false, Some(SignalId::Number(0)), "allow"),
    ];
    for (target, group, signal, expected) in signal_cases {
        let call = SignalCall {
            target,
            group,
            signal,
        };
        assert_eq!(
            rule_tag(&judge_signal(&call, &agent_ctx)),
            expected,
            "unexpected verdict for {call:?}"
        );
    }

    // judge_signal in kernel process (C20b, E8)
    let kernel_ctx = fixture(FixtureOpts {
        vantage: Some(KillGuardVantage::Process { pid: 900 }),
        ..Default::default()
    })
    .context;
    assert_eq!(
        judge_signal(
            &SignalCall {
                target: 0,
                group: true,
                signal: Some(SignalId::Number(9)),
            },
            &kernel_ctx,
        ),
        KillGuardVerdict::Allow
    );
    assert_eq!(
        judge_signal(
            &SignalCall {
                target: 0,
                group: false,
                signal: Some(SignalId::Number(15)),
            },
            &kernel_ctx,
        ),
        KillGuardVerdict::Allow
    );
    assert_eq!(
        rule_tag(&judge_signal(
            &SignalCall {
                target: AGENT,
                group: true,
                signal: Some(SignalId::Number(15)),
            },
            &kernel_ctx,
        )),
        "R4"
    );
    assert_eq!(
        rule_tag(&judge_signal(
            &SignalCall {
                target: AGENT,
                group: false,
                signal: Some(SignalId::Number(15)),
            },
            &kernel_ctx,
        )),
        "R2"
    );
}
