use std::{sync::{Arc, Mutex}, time::{Duration, Instant}};
use maho_ext_mcp::log::McpLogger;

fn warnings(logger: &McpLogger) -> usize {
    logger.get_ring_buffer().iter().filter(|line| line.contains("file sink disabled")).count()
}

#[test]
fn unwritable_sink_warns_once_skips_for_five_seconds_then_retries() {
    let root = tempfile::tempdir().unwrap();
    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();
    let mut logger = McpLogger::new("retry", &blocker, None).unwrap();
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = now.clone();
    logger.set_clock(Arc::new(move || *clock.lock().unwrap()));
    let advance = |by: Duration| *now.lock().unwrap() += by;

    logger.log("info", "one", None, None).unwrap();
    assert_eq!(warnings(&logger), 1);
    logger.log("info", "two", None, None).unwrap();
    advance(Duration::from_millis(4999));
    logger.log("info", "three", None, None).unwrap();
    assert_eq!(warnings(&logger), 1);
    assert_eq!(logger.get_ring_buffer().iter().filter(|line| line.contains("\"message\":\"three\"")).count(), 1);

    advance(Duration::from_millis(1));
    logger.log("info", "four", None, None).unwrap();
    assert_eq!(warnings(&logger), 2, "retry after the window fails again and warns again");
    advance(Duration::from_secs(4));
    logger.log("info", "five", None, None).unwrap();
    assert_eq!(warnings(&logger), 2);

    std::fs::remove_file(&blocker).unwrap();
    advance(Duration::from_secs(1));
    logger.log("info", "six", None, None).unwrap();
    assert_eq!(warnings(&logger), 2);
    let written = std::fs::read_to_string(&logger.file_path).unwrap();
    assert!(written.contains("\"message\":\"six\""));
    assert!(!written.contains("\"message\":\"five\""));
}
