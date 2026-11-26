#[tokio::test]
async fn timed_out_check_cannot_supply_the_next_verdict() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("bun")
            .arg(root.join("tests/kill_guard_timeout.js"))
            .arg(root.join("assets/kernels/js/kill-guard-bridge.js"))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("kill-guard regression test timed out")
    .expect("run Bun regression test");
    assert!(
        output.status.success(),
        "kill-guard regression failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
