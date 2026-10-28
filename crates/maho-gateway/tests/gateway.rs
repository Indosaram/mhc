//! Tests written from docs/cleanroom/specs/gateway.md §9.

use maho_ext_api::{DeliverAs, ExtensionMode, SendMessageOptions};
use maho_gateway::cli;
use maho_gateway::settings::Settings;
use maho_gateway::store::process_alive;
use maho_gateway::tools::{self, Caller};
use maho_gateway::{
    DELIVERY_SOURCE, DELIVERY_TYPE, Delivery, Endpoint, Mode, Receiver, ResolveError, Sender,
    Store, receives_in, to_message,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::path::Path;
use std::time::Duration;

fn endpoint(id: &str, name: Option<&str>, started: &str) -> Endpoint {
    Endpoint {
        version: 1,
        session_id: id.into(),
        name: name.map(str::to_owned),
        cwd: "/work".into(),
        pid: std::process::id(),
        started_at: started.into(),
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(dir.path().join("gw"));
    (dir, store)
}

/// A pid above Linux's default `pid_max`.
const DEAD_PID: u32 = 4_000_000;

#[test]
fn default_root_prefers_the_env_variable() {
    let agent = Path::new("/agent");
    assert_eq!(
        maho_gateway::default_root(agent, &|_| None),
        agent.join("gateway")
    );
    assert_eq!(
        maho_gateway::default_root(agent, &|k| (k == "MAHO_GATEWAY_DIR").then(|| "/x".into())),
        Path::new("/x")
    );
    assert_eq!(
        maho_gateway::default_root(agent, &|_| Some("  ".into())),
        agent.join("gateway")
    );
}

#[test]
fn endpoints_and_deliveries_round_trip() {
    let (_dir, store) = store();
    let a = endpoint("aaaa1111", Some("alpha"), "2026-10-28T00:00:01.000Z");
    store.register(&a).expect("register");
    assert_eq!(store.live(), vec![a]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.inbox_dir("aaaa1111"))
            .expect("inbox")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let mut first = Delivery::new("one", Sender::CommandLine { user: None }, Mode::FollowUp, 1);
    first.created_at = "2026-10-28T00:00:02.000Z".into();
    let mut second = Delivery::new("two", Sender::CommandLine { user: None }, Mode::Steer, 1);
    second.created_at = "2026-10-28T00:00:01.500Z".into();
    store.deliver("aaaa1111", &first).expect("deliver");
    store.deliver("aaaa1111", &second).expect("deliver");
    let inbox = store.inbox_dir("aaaa1111");
    std::fs::write(inbox.join(".tmp-partial"), "{").expect("tmp");
    std::fs::write(inbox.join(".hidden.json"), "{}").expect("hidden");
    std::fs::write(inbox.join("broken.json"), "not json").expect("broken");
    let pending: Vec<Delivery> = store
        .pending("aaaa1111")
        .into_iter()
        .map(|(_, d)| d)
        .collect();
    assert_eq!(pending, vec![second, first]);
    store.unregister("aaaa1111");
    assert!(store.live().is_empty());
}

#[test]
fn invalid_ids_are_refused() {
    let (_dir, store) = store();
    assert!(store.register(&endpoint("../evil", None, "t")).is_err());
    let delivery = Delivery::new("x", Sender::CommandLine { user: None }, Mode::FollowUp, 1);
    assert!(store.deliver("../evil", &delivery).is_err());
    assert!(store.pending("../evil").is_empty());
}

#[test]
fn dead_records_are_pruned() {
    let (_dir, store) = store();
    if !Path::new("/proc/self").exists() || process_alive(DEAD_PID) {
        return;
    }
    let mut dead = endpoint("deadbeef", None, "t");
    dead.pid = DEAD_PID;
    store.register(&dead).expect("register");
    assert_eq!(store.endpoints().len(), 1);
    assert!(store.live().is_empty());
    assert!(store.endpoints().is_empty());
}

#[test]
fn targets_resolve_by_id_name_and_prefix() {
    let (_dir, store) = store();
    store
        .register(&endpoint("abcd1234", Some("Builder"), "1"))
        .expect("r");
    store
        .register(&endpoint("abcd9999", Some("reviewer"), "2"))
        .expect("r");
    store.register(&endpoint("ffff0000", None, "3")).expect("r");
    let id = |target: &str| store.resolve(target, None).map(|e| e.session_id);
    assert_eq!(id("abcd1234").as_deref(), Ok("abcd1234"));
    assert_eq!(id("builder").as_deref(), Ok("abcd1234"));
    assert_eq!(id("ffff").as_deref(), Ok("ffff0000"));
    assert_eq!(id("fff"), Err(ResolveError::NotFound("fff".into())));
    assert!(matches!(id("abcd"), Err(ResolveError::Ambiguous(_, c)) if c.len() == 2));
    assert_eq!(
        store.resolve("reviewer", Some("abcd9999")),
        Err(ResolveError::SelfTarget)
    );
    assert!(
        id("nobody")
            .unwrap_err()
            .to_string()
            .contains("no live session")
    );
}

#[test]
fn sender_labels() {
    let agent = |id: &str, name: Option<&str>| Sender::Agent {
        session_id: id.into(),
        name: name.map(str::to_owned),
    };
    assert_eq!(agent("0123456789abcdef", None).label(), "session 01234567");
    assert_eq!(agent("x", Some("builder")).label(), "session builder");
    assert_eq!(
        Sender::CommandLine {
            user: Some("qa".into())
        }
        .label(),
        "qa (command line)"
    );
    assert_eq!(Sender::CommandLine { user: None }.label(), "command line");
}

#[test]
fn a_delivery_becomes_a_session_control_message() {
    let delivery = Delivery::new(
        "please rebase",
        Sender::Agent {
            session_id: "abcd1234".into(),
            name: Some("lead".into()),
        },
        Mode::Steer,
        2,
    );
    let (message, options) = to_message(&delivery);
    assert_eq!(message.custom_type, DELIVERY_TYPE);
    assert!(message.display);
    let details = message.details.clone().expect("details");
    assert_eq!(details["delivery_id"], json!(delivery.delivery_id));
    assert_eq!(details["source"], json!(DELIVERY_SOURCE));
    assert_eq!(details["deliverAs"], json!("steer"));
    assert_eq!(details["display_text"], json!("please rebase"));
    assert_eq!(details["hop"], json!(2));
    assert_eq!(details["sender"]["kind"], json!("agent"));
    assert!(options.trigger_turn);
    assert_eq!(options.deliver_as, Some(DeliverAs::Steer));
    let text = maho_gateway::model_text(&delivery);
    assert!(text.starts_with("[gateway message from session lead · hop 2 · id "));
    assert!(text.contains("not from the user"));
    assert!(text.ends_with("please rebase"));
    assert_eq!(message.content.len(), 1);
    let follow = Delivery::new("x", Sender::CommandLine { user: None }, Mode::FollowUp, 1);
    assert_eq!(to_message(&follow).1.deliver_as, Some(DeliverAs::FollowUp));
}

#[test]
fn the_receiver_delivers_each_id_once_and_keeps_failures() {
    let (_dir, store) = store();
    store.register(&endpoint("rcv00001", None, "1")).expect("r");
    let a = Delivery::new("a", Sender::CommandLine { user: None }, Mode::FollowUp, 1);
    let mut b = Delivery::new("b", Sender::CommandLine { user: None }, Mode::FollowUp, 2);
    b.created_at = "9999-12-31T00:00:00.000Z".into();
    store.deliver("rcv00001", &a).expect("d");
    let receiver = Receiver::new();
    let sent: RefCell<Vec<String>> = RefCell::default();
    let fail_next = RefCell::new(true);
    let send = |message: maho_ext_api::CustomMessage, _: SendMessageOptions| {
        if std::mem::take(&mut *fail_next.borrow_mut()) {
            return Err("not bound".to_owned());
        }
        let id = message
            .details
            .and_then(|d| d["delivery_id"].as_str().map(str::to_owned));
        sent.borrow_mut().push(id.unwrap_or_default());
        Ok(())
    };
    let outcome = receiver.poll_once(&store, "rcv00001", &send);
    assert_eq!(outcome.failed, vec![a.delivery_id.clone()]);
    assert_eq!(store.pending("rcv00001").len(), 1);
    let outcome = receiver.poll_once(&store, "rcv00001", &send);
    assert_eq!(outcome.delivered, vec![(a.delivery_id.clone(), 1)]);
    assert!(store.pending("rcv00001").is_empty());
    // The same id written again is dropped; a new one is delivered.
    store.deliver("rcv00001", &a).expect("d");
    store.deliver("rcv00001", &b).expect("d");
    let outcome = receiver.poll_once(&store, "rcv00001", &send);
    assert_eq!(outcome.delivered, vec![(b.delivery_id.clone(), 2)]);
    assert_eq!(*sent.borrow(), vec![a.delivery_id, b.delivery_id]);
    assert!(store.pending("rcv00001").is_empty());
}

fn caller(hop: u32) -> Caller {
    Caller {
        session_id: "self0001".into(),
        name: Some("me".into()),
        hop,
    }
}

fn two_sessions() -> (tempfile::TempDir, Store) {
    let (dir, store) = store();
    store
        .register(&endpoint("self0001", Some("me"), "1"))
        .expect("r");
    store
        .register(&endpoint("peer0001", Some("peer"), "2"))
        .expect("r");
    (dir, store)
}

#[test]
fn session_send_writes_an_agent_delivery() {
    let (_dir, store) = two_sessions();
    let out = tools::send(
        &store,
        &caller(0),
        3,
        &json!({"to": "peer", "message": "hi", "mode": "steer"}),
    );
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("peer [peer0001]"));
    let pending = store.pending("peer0001");
    assert_eq!(pending.len(), 1);
    let delivery = &pending[0].1;
    assert_eq!(delivery.text, "hi");
    assert_eq!(delivery.deliver_as, Mode::Steer);
    assert_eq!(delivery.hop, 1);
    assert_eq!(
        delivery.sender,
        Sender::Agent {
            session_id: "self0001".into(),
            name: Some("me".into())
        }
    );
    assert_eq!(out.details["delivery_id"], json!(delivery.delivery_id));
    let relayed = tools::send(
        &store,
        &caller(2),
        3,
        &json!({"to": "peer0001", "message": "again"}),
    );
    assert_eq!(relayed.details["hop"], json!(3));
    assert_eq!(relayed.details["mode"], json!("followUp"));
}

#[test]
fn session_send_refuses_bad_input_and_relay_loops() {
    let (_dir, store) = two_sessions();
    let cases = [
        (json!({"message": "x"}), "`to` is required"),
        (json!({"to": "peer"}), "`message` is required"),
        (
            json!({"to": "peer", "message": "x", "mode": "loud"}),
            "unknown mode",
        ),
        (
            json!({"to": "me", "message": "x"}),
            "cannot send a message to itself",
        ),
        (json!({"to": "ghost", "message": "x"}), "no live session"),
    ];
    for (params, expected) in cases {
        let out = tools::send(&store, &caller(0), 3, &params);
        assert!(
            out.is_error && out.text.contains(expected),
            "{params}: {}",
            out.text
        );
    }
    let out = tools::send(
        &store,
        &caller(3),
        3,
        &json!({"to": "peer", "message": "x"}),
    );
    assert!(
        out.is_error && out.text.contains("relay limit"),
        "{}",
        out.text
    );
    assert!(store.pending("peer0001").is_empty());
}

#[test]
fn session_list_excludes_the_caller() {
    let (_dir, store) = store();
    assert_eq!(
        tools::list(&store, "self0001").text,
        "No other live sessions."
    );
    drop(store);
    let (_dir, store) = two_sessions();
    let out = tools::list(&store, "self0001");
    assert!(out.text.starts_with("Live sessions (1):"), "{}", out.text);
    assert!(out.text.contains("peer0001 (\"peer\")"));
    assert!(!out.text.contains("self0001"));
    assert_eq!(out.details["sessions"].as_array().map(Vec::len), Some(1));
}

#[test]
fn settings_from_config_and_env() {
    let none = |_: &str| None;
    assert_eq!(Settings::from_view(None, &none), Settings::default());
    let view = json!({"gateway": {"enabled": true, "poll_ms": 10, "max_hops": 5, "extra": 1}});
    let settings = Settings::from_view(Some(&view), &none);
    assert!(settings.enabled);
    assert_eq!(settings.poll, Duration::from_millis(50));
    assert_eq!(settings.max_hops, 5);
    let off = json!({"gateway": {"enabled": false}});
    assert!(!Settings::from_view(Some(&off), &none).enabled);
    for value in ["0", "false", "OFF", "no"] {
        let env = |k: &str| (k == "MAHO_GATEWAY").then(|| value.to_owned());
        assert!(!Settings::from_view(None, &env).enabled, "{value}");
    }
    let env = |k: &str| (k == "MAHO_GATEWAY").then(|| "1".to_owned());
    assert!(Settings::from_view(None, &env).enabled);
}

#[test]
fn only_long_lived_modes_receive() {
    assert!(receives_in(ExtensionMode::Tui));
    assert!(receives_in(ExtensionMode::Rpc));
    assert!(receives_in(ExtensionMode::AppServer));
    assert!(!receives_in(ExtensionMode::Print));
    assert!(!receives_in(ExtensionMode::Json));
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn cli_list_and_send() {
    let (_dir, store) = store();
    let env = |k: &str| (k == "USER").then(|| "qa".to_owned());
    let out = cli::run(&args(&["list"]), &store, &env);
    assert_eq!((out.code, out.stdout.as_str()), (0, "No live sessions."));
    store
        .register(&endpoint("peer0001", Some("peer"), "2"))
        .expect("r");
    let out = cli::run(&args(&["list"]), &store, &env);
    assert_eq!(
        out.stdout,
        format!("peer0001\tpeer\t/work\tpid {}", std::process::id())
    );
    let out = cli::run(&args(&["list", "--json"]), &store, &env);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(parsed[0]["session_id"], json!("peer0001"));

    let out = cli::run(&args(&["send", "peer", "hello", "there"]), &store, &env);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.ends_with("-> peer0001 (followUp)"));
    let out = cli::run(
        &args(&["send", "--steer", "--from", "ops", "peer0001", "stop"]),
        &store,
        &env,
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let pending: Vec<Delivery> = store
        .pending("peer0001")
        .into_iter()
        .map(|(_, d)| d)
        .collect();
    assert_eq!(pending.len(), 2);
    let hello = pending
        .iter()
        .find(|d| d.text == "hello there")
        .expect("hello");
    assert_eq!(
        hello.sender,
        Sender::CommandLine {
            user: Some("qa".into())
        }
    );
    assert_eq!((hello.hop, hello.deliver_as), (1, Mode::FollowUp));
    let stop = pending.iter().find(|d| d.text == "stop").expect("stop");
    assert_eq!(
        stop.sender,
        Sender::CommandLine {
            user: Some("ops".into())
        }
    );
    assert_eq!(stop.deliver_as, Mode::Steer);
}

#[test]
fn cli_usage_errors() {
    let (_dir, store) = store();
    let env = |_: &str| None;
    for bad in [
        &[][..],
        &["bogus"][..],
        &["list", "--yaml"][..],
        &["send"][..],
        &["send", "peer"][..],
        &["send", "peer", "x", "--from"][..],
        &["send", "--loud", "peer", "x"][..],
    ] {
        assert_eq!(cli::run(&args(bad), &store, &env).code, 2, "{bad:?}");
    }
    let out = cli::run(&args(&["send", "ghost", "x"]), &store, &env);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("no live session"));
    assert_eq!(cli::run(&args(&["help"]), &store, &env).code, 0);
}
