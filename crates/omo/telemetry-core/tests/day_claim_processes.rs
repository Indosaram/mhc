use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const CHILD_ROOT: &str = "MAHO_TELEMETRY_TEST_CLAIM_ROOT";

#[test]
fn one_process_claims_a_shared_fresh_day() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let mut gate = [0];
        std::io::stdin().read_exact(&mut gate).expect("start signal");
        let result = telemetry_core::get_daily_active_capture_state(
            &telemetry_core::DailyActiveCaptureStateInput {
                state_dir: std::path::Path::new(&root),
                now: Some(chrono::DateTime::parse_from_rfc3339("2026-05-25T00:00:00Z")
                    .expect("fixed clock").with_timezone(&chrono::Utc)),
                diagnostics: None,
            },
        );
        println!("CAPTURE={}", result.capture_daily);
        return;
    }

    let root = tempfile::tempdir().expect("private state");
    let executable = std::env::current_exe().expect("test executable");
    let mut children = Vec::new();
    let (settled, outcomes) = mpsc::channel();
    for _ in 0..16 {
        let mut child = Command::new(&executable)
            .args(["--exact", "one_process_claims_a_shared_fresh_day", "--nocapture"])
            .env(CHILD_ROOT, root.path())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().expect("probe process");
        let mut stdout = child.stdout.take().expect("probe output");
        let sender = settled.clone();
        std::thread::spawn(move || {
            let mut output = String::new();
            let result = stdout.read_to_string(&mut output).map(|_| output);
            let _ = sender.send(result);
        });
        children.push(child);
    }
    for child in &mut children {
        child.stdin.take().expect("probe gate").write_all(&[1]).expect("start probe");
    }
    let mut captures = 0;
    let mut failures = Vec::new();
    for _ in 0..16 {
        match outcomes.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(output)) => captures += usize::from(output.contains("CAPTURE=true")),
            outcome => failures.push(format!("{outcome:?}")),
        }
    }
    for child in &mut children {
        if !failures.is_empty() {
            let _ = child.kill();
        }
        let status = child.wait().expect("probe cleanup");
        assert!(status.success(), "probe failed: {status}");
    }
    assert!(failures.is_empty(), "probe signals failed: {failures:?}");
    assert_eq!(captures, 1);
    assert!(root.path().join("daily-active.2026-05-25.claim").is_file());
}
