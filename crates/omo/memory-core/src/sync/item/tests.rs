use super::*;
use loro::{ExportMode, LoroDoc};

#[test]
fn generated_typed_items_remain_stable_over_recapture() -> Result<(), ItemError> {
    let words = [
        "plain",
        "héllo",
        "한글",
        "🙂🙂",
        "two words",
        "with: colon",
        "42",
        "true",
        "2026-01-07",
        "a\nb",
        " lead",
        "trail ",
        "\u{a0}v",
        "v\u{2028}",
    ];
    for index in 0..500 {
        let doc = document::create_item_doc(1)?;
        let word = words[index % words.len()];
        let quoted = serde_json::Value::String(word.into()).to_string();
        let text = format!(
            "---\nn: {index}\nflag: {}\nname: {quoted}\ntags: [{quoted}, 3, true]\nmeta: {{ a: 1 }}\n---\nbody {index}\n",
            index.is_multiple_of(2)
        );
        capture(&doc, "", &vec![], &text)?;
        let first = render_item(&doc)?.expect("generated live item");
        for _ in 0..5 {
            assert!(
                capture::capture_item(&doc, &first, &item_frontier(&doc), &first, None)?.is_none(),
                "generated item {index}"
            );
            assert_eq!(render_item(&doc)?.as_deref(), Some(first.as_str()));
        }
    }
    Ok(())
}

#[test]
fn disk_patch_handles_multibyte_keys_block_values_removal_and_addition() -> Result<(), ItemError> {
    let disk = "---\n茶: 'same' # keep\n# delete me\nold: |-\n  one\n  two\nn: 2\n---\nbody\n";
    let doc = document::create_item_doc(1)?;
    capture(&doc, "", &vec![], disk)?;
    current(&doc, "---\n茶: same\nn: 3\nnew: yes\n---\nbody\n")?;
    assert_eq!(
        render_for_disk(&doc, disk)?.as_deref(),
        Some("---\n茶: 'same' # keep\nn: 3\nnew: yes\n---\nbody\n")
    );
    Ok(())
}

#[test]
fn receipted_set_delta_does_not_resurrect_a_concurrent_removal()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let device = ItemDevice {
        peer: 1,
        state_dir: dir.path().to_owned(),
    };
    let doc = create_device_item_doc(&device)?;
    let text = "---\ntags: [a, b]\n---\nbody\n";
    let update = capture_on_device(&device, "x.md", &doc, "", &vec![], text, &[])?.expect("seed");
    doc.import(&update)?;
    let other = document::create_item_doc(2)?;
    other.import(&doc.export(ExportMode::all_updates())?)?;
    current(&other, "---\ntags: [a]\n---\nbody\n")?;
    doc.import(&other.export(ExportMode::all_updates())?)?;
    let update = capture_on_device(
        &device,
        "x.md",
        &doc,
        "",
        &vec![],
        "---\ntags: [a, b, c]\n---\nbody\n",
        &[],
    )?
    .expect("element delta");
    doc.import(&update)?;
    assert_eq!(
        render_item(&doc)?.as_deref(),
        Some("---\ntags:\n  - a\n  - c\n---\nbody\n")
    );
    Ok(())
}

#[test]
fn materialized_receipt_prevents_reapplying_synced_body() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let device = ItemDevice {
        peer: 1,
        state_dir: dir.path().to_owned(),
    };
    let doc = create_device_item_doc(&device)?;
    capture(&doc, "", &vec![], "base\n")?;
    let baseline = item_frontier(&doc);
    current(&doc, "base synced\n")?;
    record_materialized(&device, "x.md", &doc, &baseline, "base synced\n")?;
    let update = capture_on_device(
        &device,
        "x.md",
        &doc,
        "base\n",
        &baseline,
        "base synced more\n",
        &[],
    )?
    .expect("further typing");
    doc.import(&update)?;
    assert_eq!(render_item(&doc)?.as_deref(), Some("base synced more\n"));
    Ok(())
}

fn capture(doc: &LoroDoc, base: &str, at: &ItemFrontier, text: &str) -> Result<(), ItemError> {
    if let Some(update) = capture::capture_item(doc, base, at, text, None)? {
        doc.import(&update).map_err(super::crdt)?;
    }
    Ok(())
}
fn current(doc: &LoroDoc, text: &str) -> Result<(), ItemError> {
    capture(
        doc,
        &render::render_doc_content(doc)?,
        &item_frontier(doc),
        text,
    )
}
#[test]
fn typed_values_and_crlf_roundtrip() -> Result<(), ItemError> {
    let doc = document::create_item_doc(1)?;
    capture(
        &doc,
        "",
        &vec![],
        "---\r\ntitle: 茶 log\r\ntags: [a, 3, '3']\r\nreviewed: true\r\n---\r\nbody\r\n",
    )?;
    let text = render_item(&doc)?.ok_or(ItemError::CaptureBaseline)?;
    assert_eq!(
        text,
        "---\ntitle: 茶 log\ntags:\n  - a\n  - 3\n  - \"3\"\nreviewed: true\n---\nbody\r\n"
    );
    assert!(capture::capture_item(&doc, &text, &item_frontier(&doc), &text, None)?.is_none());
    Ok(())
}
#[test]
fn invalid_yaml_and_alias_bombs_remain_body() {
    for header in [
        "x: [unclosed",
        "x: !custom value",
        "a: &a [*a]",
        "x: 1\nx: 2",
    ] {
        let text = format!("---\n{header}\n---\nbody\n");
        let parsed = split_item(&text);
        assert!(parsed.fm.is_none());
        assert_eq!(parsed.body, text);
    }
    let text = format!("---\na: &x hi\nb: [{}]\n---\n", vec!["*x"; 65].join(", "));
    assert!(split_item(&text).fm.is_none());
}
#[test]
fn aliases_and_merge_are_classified_by_resolved_type() {
    let parsed = split_item("---\nbase: &b {n: 2, tags: [x]}\n<<: *b\ncopy: *b\n---\n");
    let fm = parsed.fm.expect("valid merged mapping");
    assert!(fm.iter().any(|e| e.key == "tags"
        && matches!(&e.value,FmValue::Set { elements,.. } if elements == &["x"])));
    assert!(
        fm.iter()
            .any(|e| e.key == "n" && e.value == FmValue::Reg { value: "2".into() })
    );
}
#[test]
fn current_and_stale_baselines_are_refused() -> Result<(), ItemError> {
    let doc = document::create_item_doc(1)?;
    capture(&doc, "", &vec![], "real\n")?;
    let at = item_frontier(&doc);
    assert!(matches!(
        capture::capture_item(&doc, "wrong", &at, "edit", None),
        Err(ItemError::CaptureBaseline)
    ));
    current(&doc, "new\n")?;
    assert!(matches!(
        capture::capture_item(&doc, "wrong", &at, "edit", None),
        Err(ItemError::CaptureBaseline)
    ));
    Ok(())
}
#[test]
fn stale_capture_preserves_unseen_edits_and_stable_peer() -> Result<(), ItemError> {
    let doc = document::create_item_doc(1)?;
    capture(&doc, "", &vec![], "alpha beta gamma\n")?;
    let at = item_frontier(&doc);
    current(&doc, "alpha FIRST beta gamma\n")?;
    capture(&doc, "alpha beta gamma\n", &at, "alpha beta gamma LAST\n")?;
    let text = render_item(&doc)?.expect("live item");
    assert!(text.contains("FIRST"));
    assert!(text.contains("LAST"));
    assert_eq!(doc.oplog_vv().len(), 1);
    Ok(())
}
#[test]
fn concurrent_register_values_converge_without_text_merging() -> Result<(), ItemError> {
    let a = document::create_item_doc(1)?;
    capture(&a, "", &vec![], "---\nx: base\n---\n")?;
    let b = document::create_item_doc(2)?;
    b.import(&a.export(ExportMode::all_updates()).map_err(crdt)?)
        .map_err(crdt)?;
    current(&a, "---\nx: apple\n---\n")?;
    current(&b, "---\nx: berry\n---\n")?;
    let ua = a.export(ExportMode::all_updates()).map_err(crdt)?;
    let ub = b.export(ExportMode::all_updates()).map_err(crdt)?;
    a.import(&ub).map_err(crdt)?;
    b.import(&ua).map_err(crdt)?;
    assert_eq!(render_item(&a)?, render_item(&b)?);
    assert_eq!(
        render_item(&a)?.as_deref(),
        Some("---\nx: \"apple / berry\"\n---\n")
    );
    Ok(())
}
#[test]
fn tombstone_covers_itself_but_not_concurrent_edits() -> Result<(), ItemError> {
    let a = document::create_item_doc(1)?;
    capture(&a, "", &vec![], "seed\n")?;
    let b = document::create_item_doc(2)?;
    b.import(&a.export(ExportMode::all_updates()).map_err(crdt)?)
        .map_err(crdt)?;
    delete_item(&a)?;
    assert!(!is_live(&a));
    assert!(render_item(&a)?.is_none());
    current(&b, "seed new\n")?;
    a.import(&b.export(ExportMode::all_updates()).map_err(crdt)?)
        .map_err(crdt)?;
    assert!(is_live(&a));
    assert!(
        render_item(&a)?
            .expect("concurrent edit lives")
            .contains("new")
    );
    Ok(())
}
#[test]
fn first_write_kind_survives_cross_kind_replacement() -> Result<(), ItemError> {
    let doc = document::create_item_doc(1)?;
    capture(&doc, "", &vec![], "---\nx: hello\n---\n")?;
    current(&doc, "---\nx: [a, b]\n---\n")?;
    assert_eq!(
        render_item(&doc)?.as_deref(),
        Some("---\nx: [ a, b ]\n---\n")
    );
    Ok(())
}
#[test]
fn key_normalization_rejects_alias_paths_and_uses_full_fold() {
    for key in [
        "", "/a", "a\\b", "a//b", "a/./b", "a/../b", "a/", "C:x", "a\0b",
    ] {
        assert_eq!(normalize_item_key(key), None);
    }
    assert_eq!(
        normalize_item_key("Straße/CAFE\u{301}.md").as_deref(),
        Some("strasse/café.md")
    );
    assert_eq!(case_fold("Σςσ İ ﬃ"), "σσσ i\u{307} ffi");
}
#[test]
fn disk_patch_preserves_comments_quotes_and_inlines_lost_anchors() -> Result<(), ItemError> {
    let disk =
        "---\nbase: &x hello\n# about other\nother: *x # alias note\nn: 2 # keep\n---\nbody\n";
    let doc = document::create_item_doc(1)?;
    capture(&doc, "", &vec![], disk)?;
    assert_eq!(render_for_disk(&doc, disk)?.as_deref(), Some(disk));
    current(&doc, "---\nbase: bye\nother: hello\nn: 2\n---\nbody\n")?;
    assert_eq!(
        render_for_disk(&doc, disk)?.as_deref(),
        Some("---\nbase: bye\n# about other\nother: hello\nn: 2 # keep\n---\nbody\n")
    );
    Ok(())
}
#[test]
fn materialization_preserves_bom_crlf_and_body_only_header() -> Result<(), ItemError> {
    let disk = "\u{feff}---\r\nx: 'hello' # keep\r\n---\r\nold\n";
    let doc = document::create_item_doc(1)?;
    capture(&doc, "", &vec![], disk)?;
    current(&doc, "---\nx: hello\n---\nnew\n")?;
    assert_eq!(
        render_for_disk(&doc, disk)?.as_deref(),
        Some("\u{feff}---\r\nx: 'hello' # keep\r\n---\r\nnew\n")
    );
    Ok(())
}
#[test]
fn malformed_receipts_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = capture_receipts::receipt_path(dir.path(), "x.md");
    std::fs::create_dir_all(path.parent().expect("capture directory"))?;
    for bad in [
        "{",
        "{}",
        "[{}]",
        "[{\"base\":[],\"text\":\"\",\"at\":[],\"stamps\":[],\"seen\":[]}]",
    ] {
        std::fs::write(&path, bad)?;
        assert!(capture_receipts::read_receipts(dir.path(), "x.md").is_err());
    }
    Ok(())
}
#[test]
fn replay_receipt_is_not_trusted_when_counters_hold_another_capture()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let device = ItemDevice {
        peer: 1,
        state_dir: dir.path().to_owned(),
    };
    let doc = create_device_item_doc(&device)?;
    let _unlanded = capture_on_device(&device, "x.md", &doc, "", &vec![], "lost", &[])?;
    capture(&doc, "", &vec![], "else")?;
    let receipts = capture_receipts::read_receipts(dir.path(), "x.md")?;
    assert!(!capture_receipts::receipt_landed(&doc, &receipts[0]));
    Ok(())
}
#[test]
fn replay_drift_and_adoption_use_one_receipted_identity() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let device = ItemDevice {
        peer: 1,
        state_dir: dir.path().to_owned(),
    };
    let doc = create_device_item_doc(&device)?;
    let values = [RegisterValue {
        key: "x".into(),
        value: "extra".into(),
    }];
    let update = capture_on_device(
        &device,
        "x.md",
        &doc,
        "",
        &vec![],
        "---\nx: own\n---\nbody\n",
        &values,
    )?
    .expect("first capture");
    doc.import(&update)?;
    assert!(
        capture_on_device(
            &device,
            "x.md",
            &doc,
            "",
            &vec![],
            "---\nx: own\n---\nbody\n",
            &values
        )?
        .is_none()
    );
    let update = capture_on_device(
        &device,
        "x.md",
        &doc,
        "",
        &vec![],
        "---\nx: own\n---\nbody more\n",
        &values,
    )?
    .expect("drift capture");
    doc.import(&update)?;
    assert_eq!(active_registers(&doc)["x"].len(), 2);
    assert!(render_item(&doc)?.expect("live").contains("body more"));
    Ok(())
}
#[test]
fn device_refuses_synced_state_and_wrong_doc_peer_and_retires_copies()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("memory");
    assert!(
        open_item_device(OpenItemDeviceOptions {
            state_dir: &root.join("state"),
            memory_root: &root,
            machine_id: Some("one")
        })
        .is_err()
    );
    let state = dir.path().join("state");
    let first = open_item_device(OpenItemDeviceOptions {
        state_dir: &state,
        memory_root: &root,
        machine_id: Some("one"),
    })?;
    let wrong = document::create_item_doc(if first.peer == 1 { 2 } else { 1 })?;
    assert!(device::assert_device_doc(&first, &wrong).is_err());
    std::fs::create_dir_all(state.join("captures"))?;
    std::fs::write(state.join("captures/old"), "old")?;
    let moved = open_item_device(OpenItemDeviceOptions {
        state_dir: &state,
        memory_root: &root,
        machine_id: Some("two"),
    })?;
    assert_ne!(first.peer, moved.peer);
    assert!(!state.join("captures").exists());
    Ok(())
}
