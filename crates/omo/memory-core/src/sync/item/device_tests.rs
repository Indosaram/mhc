use super::*;

#[test]
fn device_peer_is_persisted_and_concurrent_first_opens_leave_no_candidates() -> Result<(), Box<dyn std::error::Error>> {
    // Given an unused device directory, and threads released by one exact barrier.
    let dir = tempfile::tempdir()?;
    let state = dir.path().join("state");
    let memory = dir.path().join("memory");
    let barrier = std::sync::Barrier::new(64);
    // When all first opens contend for publication.
    let devices = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..64).map(|_| scope.spawn(|| {
            barrier.wait();
            open_item_device(OpenItemDeviceOptions { state_dir: &state, memory_root: &memory, machine_id: Some("fixed-machine") })
        })).collect();
        handles.into_iter().map(|handle| handle.join().expect("open worker")).collect::<Result<Vec<_>, _>>()
    })?;
    // Then one nonzero identity is reused and all candidate directories are gone.
    let peer = devices[0].peer;
    assert_ne!(peer, 0);
    assert!(devices.iter().all(|device| device.peer == peer));
    let names = std::fs::read_dir(&state)?.map(|entry| entry.map(|e| e.file_name())).collect::<Result<Vec<_>, _>>()?;
    assert_eq!(names, [std::ffi::OsString::from("peer")]);
    assert_eq!(open_item_device(OpenItemDeviceOptions { state_dir: &state, memory_root: &memory, machine_id: Some("fixed-machine") })?.peer, peer);
    Ok(())
}

#[test]
fn copied_state_path_gets_new_identity_but_original_path_restore_keeps_it() -> Result<(), Box<dyn std::error::Error>> {
    // Given a device identity copied to a sibling profile on the same machine.
    let dir = tempfile::tempdir()?;
    let memory = dir.path().join("memory");
    let state = dir.path().join("original");
    let copy = dir.path().join("copy");
    let original = open_item_device(OpenItemDeviceOptions { state_dir: &state, memory_root: &memory, machine_id: Some("same") })?;
    std::fs::create_dir_all(copy.join("peer"))?;
    let id = std::fs::read(state.join("peer/id"))?;
    let machine = std::fs::read(state.join("peer/machine"))?;
    std::fs::write(copy.join("peer/id"), &id)?;
    std::fs::write(copy.join("peer/machine"), &machine)?;
    // When the copy is opened and the original identity is restored at its original path.
    let sibling = open_item_device(OpenItemDeviceOptions { state_dir: &copy, memory_root: &memory, machine_id: Some("same") })?;
    std::fs::remove_dir_all(state.join("peer"))?;
    std::fs::create_dir(state.join("peer"))?;
    std::fs::write(state.join("peer/id"), id)?;
    std::fs::write(state.join("peer/machine"), machine)?;
    let restored = open_item_device(OpenItemDeviceOptions { state_dir: &state, memory_root: &memory, machine_id: Some("same") })?;
    // Then a new path cannot reuse counters while an in-place restore can.
    assert_ne!(sibling.peer, original.peer);
    assert_eq!(restored.peer, original.peer);
    Ok(())
}

#[test]
fn synced_state_paths_and_corrupt_peer_records_are_refused() -> Result<(), Box<dyn std::error::Error>> {
    // Given several paths in the synced tree.
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("memory");
    for state in [root.clone(), root.join(".git/omo-sync"), root.join("nested/state")] {
        // When opened as device state.
        let result = open_item_device(OpenItemDeviceOptions { state_dir: &state, memory_root: &root, machine_id: Some("machine") });
        // Then they cannot leak identity into synchronization.
        assert!(matches!(result, Err(ItemError::DevicePeer(_))));
    }
    let state = dir.path().join("memory-state");
    std::fs::create_dir_all(state.join("peer"))?;
    for bad in ["not-a-peer\n", "18446744073709551616\n"] {
        std::fs::write(state.join("peer/id"), bad)?;
        assert!(matches!(open_item_device(OpenItemDeviceOptions { state_dir: &state, memory_root: &root, machine_id: Some("machine") }), Err(ItemError::DevicePeer(_))));
        assert_eq!(std::fs::read_to_string(state.join("peer/id"))?, bad);
    }
    Ok(())
}

#[test]
fn receipt_retention_keeps_only_the_newest_baselines() -> Result<(), Box<dyn std::error::Error>> {
    // Given more receipts than the bounded per-item history permits.
    let dir = tempfile::tempdir()?;
    let receipts: Vec<_> = (0..40).map(|index| capture_receipts::CaptureReceipt { base: vec![], text: index.to_string(), at: vec![], stamps: vec![], seen: vec![], span: None }).collect();
    // When persisted and loaded at the actual receipt boundary.
    capture_receipts::write_receipts(dir.path(), "x.md", &receipts)?;
    let loaded = capture_receipts::read_receipts(dir.path(), "x.md")?;
    // Then exactly the newest 32 remain, isolated from another item key.
    assert_eq!(loaded.len(), capture_receipts::MAX_RECEIPTS);
    assert_eq!(loaded.first().expect("first").text, "8");
    assert_eq!(loaded.last().expect("last").text, "39");
    assert!(capture_receipts::read_receipts(dir.path(), "other.md")?.is_empty());
    Ok(())
}

#[test]
fn unicode_casefold_and_path_normalization_cover_non_bmp_and_nfc_edges() {
    // Given the upstream casefold and key edge corpus.
    let folds = [("", ""), ("Notes.MD", "notes.md"), ("Straße", "strasse"), ("ﬁles", "files"), ("İSTANBUL", "i\u{307}stanbul"), ("ıI", "ıi"), ("𐐀𐐨", "𐐨𐐨"), ("\u{ab88}", "\u{13b8}"), ("\u{13f8}", "\u{13f0}"), ("a\u{301}", "a\u{301}"), ("한글", "한글")];
    for (input, expected) in folds {
        // When full folding is applied.
        let folded = case_fold(input);
        // Then it changes case only, including supplementary and Cherokee characters.
        assert_eq!(folded, expected);
        assert_eq!(case_fold(&folded), folded);
    }
    for key in ["..x/..y.md", ".hidden/file.md", "a.../b....md"] {
        assert_eq!(normalize_item_key(key).as_deref(), Some(key));
    }
    assert_eq!(normalize_item_key("ǰ.md"), normalize_item_key("J\u{30c}.md"));
    assert_eq!(normalize_item_key("Σίσυφος.md"), normalize_item_key("σίσυφοσ.md"));
    assert_eq!(normalize_item_key("people/Hojin/Card.MD").as_deref(), Some("people/hojin/card.md"));
}

#[test]
fn copied_memory_does_not_copy_the_device_identity() -> Result<(), Box<dyn std::error::Error>> {
    // Given memory content copied without the separate device state directory.
    let dir = tempfile::tempdir()?;
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(a.join("memory"))?;
    std::fs::create_dir_all(b.join("memory"))?;
    std::fs::write(a.join("memory/note.md"), "body\n")?;
    std::fs::copy(a.join("memory/note.md"), b.join("memory/note.md"))?;
    // When each machine opens its own device state.
    let first = open_item_device(OpenItemDeviceOptions { state_dir: &a.join("state"), memory_root: &a.join("memory"), machine_id: Some("machine-a") })?;
    let second = open_item_device(OpenItemDeviceOptions { state_dir: &b.join("state"), memory_root: &b.join("memory"), machine_id: Some("machine-b") })?;
    // Then identity stays outside memory and every exported capture uses its device peer.
    assert_ne!(first.peer, second.peer);
    let names = std::fs::read_dir(b.join("memory"))?.map(|entry| entry.map(|e| e.file_name())).collect::<Result<Vec<_>, _>>()?;
    assert_eq!(names, [std::ffi::OsString::from("note.md")]);
    let doc = create_device_item_doc(&second)?;
    super::capture_tests::fire(&second, &doc, "", &vec![], "hello\n")?;
    assert_eq!(doc.oplog_vv().len(), 1);
    assert!(doc.oplog_vv().get(&second.peer).is_some());
    Ok(())
}
