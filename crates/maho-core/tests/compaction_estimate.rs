#[test]
fn public_estimator_matches_pinned_source_for_supplementary_text() {
    let output = std::process::Command::new("bun")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/golden/compaction-estimate.mjs"))
        .env("SENPI_SRC", std::env::var("SENPI_SRC").unwrap_or_else(|_| "/home/indo/code/senpi".into()))
        .output().expect("pinned source estimator");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let cases: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).expect("source results");
    assert_eq!(cases.len(), 10);
    for case in cases {
        assert_eq!(maho_core::compaction::compaction::estimate_tokens(&case["message"]),
            case["tokens"].as_u64().expect("token count"), "{}", case["message"]);
    }
}
