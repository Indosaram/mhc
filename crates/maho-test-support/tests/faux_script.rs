use maho_test_support::faux::{FauxError, FauxQueue, NO_MORE_RESPONSES, load_script};

#[test]
fn hello_script_loads_and_drains_in_order() {
    let script = load_script("hello").expect("hello script");
    assert_eq!(script.prompt, "Say hello.");
    let mut queue = FauxQueue::from_script(&script);
    assert_eq!(queue.pending(), 1);
    let response = queue.next_response().expect("first response");
    assert_eq!(response.content, "Hello from the faux provider.");
    assert_eq!(response.stop_reason, "stop");
    let error = queue.next_response().expect_err("queue is empty");
    assert!(matches!(error, FauxError::Exhausted));
    assert_eq!(error.to_string(), NO_MORE_RESPONSES);
    assert_eq!(queue.call_count(), 2);
}

#[test]
fn appended_responses_follow_queued_ones() {
    let script = load_script("hello").expect("hello script");
    let mut queue = FauxQueue::default();
    queue.append(script.responses.clone());
    queue.append(script.responses);
    assert_eq!(queue.pending(), 2);
}

#[test]
fn script_names_cannot_escape_the_scripts_dir() {
    for bad in ["../hello", "", "-x", "Hello", "a/b"] {
        assert!(matches!(load_script(bad), Err(FauxError::InvalidName(_))), "{bad}");
    }
    assert!(matches!(load_script("missing-script"), Err(FauxError::Read { .. })));
}
