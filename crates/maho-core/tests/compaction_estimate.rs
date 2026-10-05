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

#[test]
fn pinned_large_session_migrates_projects_context_and_keeps_a_valid_cut_boundary() {
    let source = std::env::var("SENPI_SRC").unwrap_or_else(|_| "/home/indo/code/senpi".into());
    let fixture = std::path::Path::new(&source).join("packages/coding-agent/test/fixtures/large-session.jsonl");
    let content = std::fs::read_to_string(fixture).expect("pinned large-session fixture");
    let mut entries = maho_core::session_manager::parse_session_entries(&content);
    maho_core::session_manager::migrate_session_entries(&mut entries);
    entries.retain(|entry| entry["type"] != "session");
    assert!(entries.len() > 100);
    assert!(entries.iter().filter(|entry| entry["type"] == "message").count() > 100);
    let settings = maho_core::compaction::settings::default_compaction_settings();
    let cut = maho_core::compaction::compaction::find_cut_point(&entries, 0, entries.len(), settings.keep_recent_tokens);
    let boundary = &entries[cut.first_kept_entry_index];
    assert_eq!(boundary["type"], "message");
    assert!(matches!(boundary["message"]["role"].as_str(), Some("user" | "assistant")));
    let context = maho_core::session_manager::build_session_context(&entries, None);
    assert!(context.messages.len() > 100);
    assert!(context.model.is_some());
}
