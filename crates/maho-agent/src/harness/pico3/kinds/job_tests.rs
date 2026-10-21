use super::*;

#[test]
fn occurrence_key_is_stable_across_recovery_and_changes_for_recurrence() {
    assert_eq!(key(42, 1), "42:1");
    assert_eq!(key(42, 1), key(42, 1));
    assert_ne!(key(42, 1), key(42, 2));
}

#[test]
fn process_checkpoint_retains_exact_occurrence_key() {
    for phase in ["spawning", "running"] {
        let cp: JobCheckpoint = serde_json::from_value(serde_json::json!({"phase":phase,"key":"42:7","occurrence":7})).expect("checkpoint");
        let durable = checkpoint(&cp).expect("durable");
        assert_eq!(durable.phase, phase);
        assert_eq!(durable.fields.get("key"), Some(&json(&"42:7").expect("key")));
        assert_eq!(durable.fields.get("occurrence"), Some(&json(&7).expect("occurrence")));
    }
}

#[test]
fn output_projects_exit_and_dropped_counts_without_inventing_success() {
    let status = ProcessStatus::Exited { exit_code: 17, stdout: "out".into(), stderr: "err".into(), dropped_stdout: 3, dropped_stderr: 9 };
    let actual = output(&status).expect("exited output");
    assert_eq!(actual.exit_code, Some(17));
    assert_eq!(actual.stdout.as_deref(), Some("out"));
    assert_eq!(actual.stderr.as_deref(), Some("err"));
    assert_eq!((actual.dropped_stdout, actual.dropped_stderr), (Some(3), Some(9)));
    assert!(output(&ProcessStatus::Unknown).is_none());
}

#[test]
fn input_preserves_native_process_spec_and_scheduling_fields() {
    let input: JobInput = serde_json::from_value(serde_json::json!({"command":"bash","args":["-c","exit 3"],"cwd":"/tmp","env":{"A":"B"},"notify":true,"rerun":false,"every":500,"notBefore":900})).expect("input");
    assert_eq!(input.process.command, "bash");
    assert_eq!(input.process.args, ["-c", "exit 3"]);
    assert_eq!(input.process.cwd, "/tmp");
    assert_eq!(input.process.env.as_ref().and_then(|env| env.get("A")).map(String::as_str), Some("B"));
    assert_eq!((input.every, input.not_before), (Some(500), Some(900)));
}

#[test]
fn production_status_backoff_is_bounded() {
    assert_eq!((0..7).map(poll_delay).collect::<Vec<_>>(), [100, 200, 400, 800, 1000, 1000, 1000]);
}

#[test]
fn unknown_process_reruns_same_occurrence_only_when_explicitly_allowed() {
    assert!(matches!(unknown(true, 7), Poll::Rerun(7)));
    assert!(matches!(unknown(false, 7), Poll::Step(Step::Done(_))));
}

struct Host { events: std::rc::Rc<std::cell::RefCell<Vec<String>>> }
impl ProcessHost for Host {
    fn start<'a>(&'a self, _: &'a str, _: ProcessSpec, _: &'a Context) -> LocalBoxFuture<'a, Result<()>> { Box::pin(async { panic!("abort must not start") }) }
    fn status<'a>(&'a self, _: &'a str, _: &'a Context) -> LocalBoxFuture<'a, Result<ProcessStatus>> { Box::pin(async { panic!("abort does not query status") }) }
    fn kill<'a>(&'a self, key: &'a str, signal: ProcessSignal, _: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move { self.events.borrow_mut().push(format!("{key}:{signal:?}")); Ok(()) })
    }
}

#[tokio::test]
async fn abort_orders_term_grace_and_kill_at_exact_deadline() {
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let host = Host { events: events.clone() };
    let ctx = crate::harness::context::background_context();
    let during = events.clone();
    terminate(&host, "42:7", &ctx, || 10_000, |deadline| Box::pin(async move {
        assert_eq!(during.borrow().as_slice(), ["42:7:Sigterm"]);
        during.borrow_mut().push(format!("grace:{deadline}"));
        Ok(())
    })).await.expect("abort");
    assert_eq!(events.borrow().as_slice(), ["42:7:Sigterm", "grace:15000", "42:7:Sigkill"]);
}
