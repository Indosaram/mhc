//! Source ports of capture, capture.r3, capture-fields.r6 and tombstone cases.
use super::*;
use loro::{ExportMode, LoroDoc};

pub(super) fn sync(a: &LoroDoc, b: &LoroDoc) -> Result<(), ItemError> {
    let ua = a.export(ExportMode::updates(&b.oplog_vv())).map_err(crdt)?;
    let ub = b.export(ExportMode::updates(&a.oplog_vv())).map_err(crdt)?;
    a.import(&ub).map_err(crdt)?;
    b.import(&ua).map_err(crdt)?;
    Ok(())
}

pub(super) fn capture(doc: &LoroDoc, base: &str, at: &ItemFrontier, text: &str) -> Result<(), ItemError> {
    if let Some(update) = capture::capture_item(doc, base, at, text, None)? {
        doc.import(&update).map_err(crdt)?;
    }
    Ok(())
}

pub(super) fn current(doc: &LoroDoc, text: &str) -> Result<(), ItemError> {
    capture(doc, &render::render_doc_content(doc)?, &item_frontier(doc), text)
}

pub(super) fn fire(device: &ItemDevice, doc: &LoroDoc, base: &str, at: &ItemFrontier, text: &str) -> Result<(), ItemError> {
    if let Some(update) = capture_on_device(device, "item.md", doc, base, at, text, &[])? {
        doc.import(&update).map_err(crdt)?;
    }
    Ok(())
}

#[test]
fn transplant_assigns_fresh_stamps_in_root_and_tag_order() -> Result<(), ItemError> {
    // Given merged maps whose insertion order differs from lexical order.
    let merged = create_item_doc(7)?;
    merged.get_map("fm\u{1f}z\u{1f}set").insert("z-tag", "last").map_err(crdt)?;
    merged.get_map("fm\u{1f}a\u{1f}set").insert("z-tag", "second").map_err(crdt)?;
    merged.get_map("fm\u{1f}a\u{1f}set").insert("a-tag", "first").map_err(crdt)?;
    merged.commit();
    let target = create_item_doc(1)?;
    // When merged contents become stable-device operations.
    transplant::transplant_content(&target, &merged)?;
    target.commit();
    // Then hash traversal cannot determine key or element first-write stamps.
    assert_eq!(render_item(&target)?.as_deref(), Some("---\na:\n  - first\n  - second\nz:\n  - last\n---\n"));
    Ok(())
}

#[test]
fn stale_capture_replays_identical_history_with_identical_operations() -> Result<(), ItemError> {
    // Given a stale baseline and a newer write from peer zero.
    let doc = create_item_doc(0)?;
    current(&doc, "base\n")?;
    let at = item_frontier(&doc);
    current(&doc, "base\nnewer\n")?;
    let working = "---\nz: [last, first]\na: value\n---\nbase\n";
    // When the same history is captured on two independently allocated forks.
    let a = doc.fork();
    let b = doc.fork();
    a.set_peer_id(0).map_err(crdt)?;
    b.set_peer_id(0).map_err(crdt)?;
    capture(&a, "base\n", &at, working)?;
    capture(&b, "base\n", &at, working)?;
    // Then fresh operations, tags and stamps agree, not only rendered values.
    let changes = |doc: &LoroDoc| doc.export_json_updates_without_peer_compression(&loro::VersionVector::new(), &doc.oplog_vv());
    assert_eq!(serde_json::to_value(changes(&a)).map_err(crdt)?, serde_json::to_value(changes(&b)).map_err(crdt)?);
    assert_eq!(render_item(&a)?, render_item(&b)?);
    assert!(a.oplog_vv().iter().all(|(peer, _)| *peer == 0));
    Ok(())
}

#[test]
fn capture_returns_an_update_without_mutating_the_source() -> Result<(), ItemError> {
    // Given an empty device document.
    let doc = create_item_doc(1)?;
    let text = "---\ndescription: hello\n---\nbody text\n";
    // When a new item is captured, but not imported.
    let update = capture::capture_item(&doc, "", &vec![], text, None)?.expect("new item update");
    // Then the source remains empty until the engine imports it.
    assert!(render_item(&doc)?.is_none());
    doc.import(&update).map_err(crdt)?;
    assert_eq!(render_item(&doc)?.as_deref(), Some(text));
    Ok(())
}

#[test]
fn valid_frontmatter_can_be_replaced_by_invalid_yaml_without_losing_bytes() -> Result<(), ItemError> {
    // Given a typed item.
    let doc = create_item_doc(1)?;
    current(&doc, "---\ndescription: hello\ncount: 2\n---\nbody\n")?;
    let bad = "---\ndescription: [unclosed\n---\nbody\n";
    // When the user breaks its YAML.
    current(&doc, bad)?;
    // Then the old keys disappear and the entire file becomes body.
    assert_eq!(render_item(&doc)?.as_deref(), Some(bad));
    assert!(render::frontmatter_entries(&doc)?.is_empty());
    Ok(())
}

#[test]
fn stale_wholesale_body_replacement_keeps_unseen_inserts() -> Result<(), ItemError> {
    // Given a stale baseline and a newer insertion.
    let doc = create_item_doc(1)?;
    current(&doc, "old body\n")?;
    let at = item_frontier(&doc);
    current(&doc, "NEW old body\n")?;
    // When the stale writer replaces all the body it saw.
    capture(&doc, "old body\n", &at, "fresh\n")?;
    // Then both the unseen insert and replacement survive.
    let text = render_item(&doc)?.expect("live");
    assert!(text.contains("NEW"));
    assert!(text.contains("fresh"));
    Ok(())
}

#[test]
fn sequential_and_interleaved_stale_captures_keep_each_written_marker() -> Result<(), ItemError> {
    for trial in 0..40 {
        // Given two devices sharing one baseline.
        let a = create_item_doc(1)?;
        let b = create_item_doc(2)?;
        current(&a, "seed\n")?;
        sync(&a, &b)?;
        let at = item_frontier(&a);
        let mut markers = Vec::new();
        // When both devices repeatedly capture from that old baseline.
        for round in 0..(2 + trial % 4) {
            for (prefix, doc) in [("A", &a), ("B", &b)] {
                let marker = format!("{prefix}{trial}_{round}q");
                capture(doc, "seed\n", &at, &format!("seed {marker}\n"))?;
                markers.push(marker);
            }
            sync(&a, &b)?;
        }
        // Then delivery converges and no earlier edit was lost.
        let text = render_item(&a)?.expect("live");
        assert_eq!(render_item(&b)?.as_deref(), Some(text.as_str()));
        for marker in markers {
            assert!(text.contains(&marker), "trial {trial}: {marker}");
        }
        assert_eq!(a.oplog_vv().len(), 2);
    }
    Ok(())
}

#[test]
fn device_double_fires_and_drifted_replays_apply_each_edit_once() -> Result<(), Box<dyn std::error::Error>> {
    for seed in ["hello\n", "---\ndescription: hi\n---\nhello\n"] {
        // Given a receipted initial body or typed file.
        let dir = tempfile::tempdir()?;
        let device = ItemDevice { peer: 1, state_dir: dir.path().into() };
        let doc = create_device_item_doc(&device)?;
        fire(&device, &doc, "", &vec![], seed)?;
        let at = item_frontier(&doc);
        let mut disk = seed.to_owned();
        // When every drifted edit fires twice against the original baseline.
        for word in ["<A>", "<B>", "<C>", "<D>"] {
            disk.push_str(&format!("{word}\n"));
            fire(&device, &doc, seed, &at, &disk)?;
            assert!(capture_on_device(&device, "item.md", &doc, seed, &at, &disk, &[])?.is_none());
        }
        // Then body markers and frontmatter remain byte-exact.
        assert_eq!(render_item(&doc)?.as_deref(), Some(disk.as_str()));
    }
    Ok(())
}

#[test]
fn frontmatter_double_fire_does_not_duplicate_a_register() -> Result<(), Box<dyn std::error::Error>> {
    // Given one receipted register.
    let dir = tempfile::tempdir()?;
    let device = ItemDevice { peer: 1, state_dir: dir.path().into() };
    let doc = create_device_item_doc(&device)?;
    let seed = "---\ndescription: hi\n---\n";
    fire(&device, &doc, "", &vec![], seed)?;
    let at = item_frontier(&doc);
    let next = "---\ndescription: hi there\n---\n";
    // When the edited frontmatter fires twice.
    fire(&device, &doc, seed, &at, next)?;
    fire(&device, &doc, seed, &at, next)?;
    // Then only the whole edited value is active.
    assert_eq!(render_item(&doc)?.as_deref(), Some(next));
    assert_eq!(active_registers(&doc)["description"], ["hi there"]);
    Ok(())
}

#[test]
fn crash_before_import_recaptures_the_complete_drifted_text() -> Result<(), Box<dyn std::error::Error>> {
    // Given a receipt whose update never reached the source document.
    let dir = tempfile::tempdir()?;
    let device = ItemDevice { peer: 1, state_dir: dir.path().into() };
    let doc = create_device_item_doc(&device)?;
    fire(&device, &doc, "", &vec![], "base\n")?;
    let at = item_frontier(&doc);
    let dropped = capture_on_device(&device, "item.md", &doc, "base\n", &at, "base\n<A>\n", &[])?;
    assert!(dropped.is_some());
    // When the user types more before replay.
    fire(&device, &doc, "base\n", &at, "base\n<A>\n<B>\n")?;
    // Then neither the lost update nor the drift is duplicated or omitted.
    assert_eq!(render_item(&doc)?.as_deref(), Some("base\n<A>\n<B>\n"));
    Ok(())
}

#[test]
fn older_landed_baseline_replay_does_not_undo_a_newer_capture() -> Result<(), Box<dyn std::error::Error>> {
    // Given edits captured under two successive baselines.
    let dir = tempfile::tempdir()?;
    let device = ItemDevice { peer: 1, state_dir: dir.path().into() };
    let doc = create_device_item_doc(&device)?;
    fire(&device, &doc, "", &vec![], "x\n")?;
    let at = item_frontier(&doc);
    fire(&device, &doc, "x\n", &at, "x\n<A>\n")?;
    fire(&device, &doc, "x\n<A>\n", &item_frontier(&doc), "x\n<A>\n<B>\n")?;
    // When the older edit is replayed.
    let replay = capture_on_device(&device, "item.md", &doc, "x\n", &at, "x\n<A>\n", &[])?;
    // Then the replay is empty and the newer text remains.
    assert!(replay.is_none());
    assert_eq!(render_item(&doc)?.as_deref(), Some("x\n<A>\n<B>\n"));
    Ok(())
}

#[test]
fn hand_resolution_survives_sync_and_recapture_on_both_devices() -> Result<(), Box<dyn std::error::Error>> {
    // Given independently written register values.
    let dir = tempfile::tempdir()?;
    let da = ItemDevice { peer: 1, state_dir: dir.path().join("a") };
    let db = ItemDevice { peer: 2, state_dir: dir.path().join("b") };
    let a = create_device_item_doc(&da)?;
    let b = create_device_item_doc(&db)?;
    fire(&da, &a, "", &vec![], "---\nn: 2\n---\n")?;
    fire(&db, &b, "", &vec![], "---\nn: 3\n---\n")?;
    sync(&a, &b)?;
    let conflict = render_item(&a)?.expect("conflict");
    // When a writer resolves the conflict to one previously held value.
    let resolved = "---\nn: 2\n---\n";
    fire(&da, &a, &conflict, &item_frontier(&a), resolved)?;
    sync(&a, &b)?;
    // Then both peers keep the resolution and settle without new operations.
    for (device, doc) in [(&da, &a), (&db, &b)] {
        assert_eq!(render_item(doc)?.as_deref(), Some(resolved));
        assert!(capture_on_device(device, "item.md", doc, resolved, &item_frontier(doc), resolved, &[])?.is_none());
    }
    Ok(())
}

#[test]
fn drifted_field_edits_preserve_unseen_whole_values() -> Result<(), Box<dyn std::error::Error>> {
    // Given three devices, two of which edit registers while B retains the seed on disk.
    let dir = tempfile::tempdir()?;
    let devices: Vec<_> = (1..=3).map(|peer| ItemDevice { peer, state_dir: dir.path().join(peer.to_string()) }).collect();
    let a = create_device_item_doc(&devices[0])?;
    let b = create_device_item_doc(&devices[1])?;
    let c = create_device_item_doc(&devices[2])?;
    let seed = "---\nn: 1\nf: true\ndescription: the bird sat\n---\nbody\n";
    fire(&devices[0], &a, "", &vec![], seed)?;
    sync(&a, &b)?;
    sync(&a, &c)?;
    let at = item_frontier(&b);
    fire(&devices[0], &a, seed, &item_frontier(&a), "---\nn: 2\nf: false\ndescription: the red bird sat\n---\nbody\n")?;
    fire(&devices[2], &c, seed, &item_frontier(&c), "---\nn: 3\nf: 0\ndescription: the bird sat down\n---\nbody\n")?;
    sync(&a, &c)?;
    sync(&a, &b)?;
    // When B captures a body edit, then drifts the numeric register to a string.
    fire(&devices[1], &b, seed, &at, &format!("{seed}<X>\n"))?;
    fire(&devices[1], &b, seed, &at, "---\nn: hello\nf: true\ndescription: the bird sat\n---\nbody\n<X>\n")?;
    sync(&a, &b)?;
    sync(&b, &c)?;
    // Then no value was spliced or superseded without having been seen.
    let values = active_registers(&b);
    assert_eq!(values["n"], ["2", "3", "hello"]);
    assert_eq!(values["f"], ["0", "false"]);
    assert_eq!(values["description"], ["the bird sat down", "the red bird sat"]);
    let text = render_item(&b)?.expect("live");
    assert_eq!(text.matches("<X>").count(), 1);
    assert_eq!(render_item(&a)?, render_item(&b)?);
    assert_eq!(render_item(&c)?, render_item(&b)?);
    Ok(())
}

#[test]
fn drifted_list_to_string_replaces_seen_elements_only() -> Result<(), Box<dyn std::error::Error>> {
    // Given a concurrent addition unknown to the stale disk writer.
    let dir = tempfile::tempdir()?;
    let da = ItemDevice { peer: 1, state_dir: dir.path().join("a") };
    let db = ItemDevice { peer: 2, state_dir: dir.path().join("b") };
    let a = create_device_item_doc(&da)?;
    let b = create_device_item_doc(&db)?;
    let seed = "---\ntags:\n  - a\n---\nbody\n";
    fire(&da, &a, "", &vec![], seed)?;
    sync(&a, &b)?;
    let at = item_frontier(&b);
    fire(&da, &a, seed, &item_frontier(&a), "---\ntags: [a, c]\n---\nbody\n")?;
    sync(&a, &b)?;
    fire(&db, &b, seed, &at, &format!("{seed}<X>\n"))?;
    // When the stale writer replaces its seen list with one string.
    fire(&db, &b, seed, &at, "---\ntags: 'x: y'\n---\nbody\n<X>\n")?;
    sync(&a, &b)?;
    // Then the unseen element remains beside the whole replacement string.
    let fields = render::frontmatter_entries(&b)?;
    let (_, yaml::Value::Seq(tags)) = &fields[0] else { panic!("set-kind tags") };
    assert!(tags.contains(&yaml::Value::String("c".into())));
    assert!(tags.contains(&yaml::Value::String("x: y".into())));
    assert_eq!(tags.len(), 2);
    assert_eq!(render_item(&a)?, render_item(&b)?);
    Ok(())
}

#[test]
fn materialized_register_is_seen_and_can_be_replaced_after_a_crash() -> Result<(), Box<dyn std::error::Error>> {
    // Given a synced register written to disk without saving the new baseline.
    let dir = tempfile::tempdir()?;
    let device = ItemDevice { peer: 2, state_dir: dir.path().into() };
    let a = create_item_doc(1)?;
    let b = create_device_item_doc(&device)?;
    let seed = "---\nn: 1\n---\n";
    current(&a, seed)?;
    sync(&a, &b)?;
    let at = item_frontier(&b);
    current(&a, "---\nn: 2\n---\n")?;
    sync(&a, &b)?;
    record_materialized(&device, "item.md", &b, &at, &render_item(&b)?.expect("written"))?;
    // When the user overwrites the value they actually saw.
    fire(&device, &b, seed, &at, "---\nn: 5\n---\n")?;
    sync(&a, &b)?;
    // Then the materialized value is superseded, not retained as an unseen conflict.
    assert_eq!(render_item(&a)?.as_deref(), Some("---\nn: 5\n---\n"));
    assert_eq!(render_item(&a)?, render_item(&b)?);
    Ok(())
}

#[test]
fn tombstones_cover_observed_edits_and_concurrent_deletes() -> Result<(), ItemError> {
    for simultaneous in [false, true] {
        // Given a shared item and a second peer's observed edit.
        let a = create_item_doc(1)?;
        let b = create_item_doc(2)?;
        current(&a, "v1\n")?;
        sync(&a, &b)?;
        current(&b, "v1\nv2\n")?;
        sync(&a, &b)?;
        // When one or both peers delete it.
        delete_item(&a)?;
        if simultaneous { delete_item(&b)?; }
        sync(&a, &b)?;
        // Then neither the known edit nor the other delete revives it.
        assert!(!is_live(&a));
        assert!(!is_live(&b));
        assert!(render_item(&a)?.is_none());
        assert!(render_for_disk(&b, "v1\nv2\n")?.is_none());
    }
    Ok(())
}

#[test]
fn unchanged_stale_capture_does_not_revive_but_recreation_does() -> Result<(), ItemError> {
    // Given an item deleted after its baseline was saved.
    let doc = create_item_doc(1)?;
    let old = "---\ndescription: d\n---\nold\n";
    current(&doc, old)?;
    let at = item_frontier(&doc);
    delete_item(&doc)?;
    // When an unchanged stale capture is attempted, followed by a real recreation.
    assert!(capture::capture_item(&doc, old, &at, old, None)?.is_none());
    assert!(!is_live(&doc));
    capture(&doc, old, &item_frontier(&doc), "---\ndescription: d\n---\nnew\n")?;
    // Then only the new content is live.
    assert_eq!(render_item(&doc)?.as_deref(), Some("---\ndescription: d\n---\nnew\n"));
    Ok(())
}

#[test]
fn adoption_replay_never_reintroduces_a_superseded_value() -> Result<(), Box<dyn std::error::Error>> {
    // Given an adopted value that a later user write superseded.
    let dir = tempfile::tempdir()?;
    let device = ItemDevice { peer: 1, state_dir: dir.path().into() };
    let doc = create_device_item_doc(&device)?;
    let values = [RegisterValue { key: "n".into(), value: "2".into() }];
    let update = capture_on_device(&device, "item.md", &doc, "", &vec![], "---\nn: 1\n---\n", &values)?.expect("adoption");
    // The receipt is durable before the update is returned, but not yet landed.
    let receipts = capture_receipts::read_receipts(dir.path(), "item.md")?;
    assert!(!capture_receipts::receipt_landed(&doc, &receipts[0]));
    doc.import(&update)?;
    assert!(capture_receipts::receipt_landed(&doc, &receipts[0]));
    let base = render_item(&doc)?.expect("joined");
    fire(&device, &doc, &base, &item_frontier(&doc), "---\nn: 3\n---\n")?;
    // When the same stale adoption is requested again.
    let text = render_item(&doc)?.expect("resolved");
    let update = capture_on_device(&device, "item.md", &doc, &text, &item_frontier(&doc), &text, &values)?;
    // Then the historical write counts as written, but is not active again.
    assert!(update.is_none());
    assert!(adopt_values::register_values_written(&doc, &values));
    assert_eq!(active_registers(&doc)["n"], ["3"]);
    Ok(())
}
