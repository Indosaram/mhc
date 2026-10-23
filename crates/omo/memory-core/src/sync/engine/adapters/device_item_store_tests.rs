use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    state: std::path::PathBuf,
    memory: std::path::PathBuf,
    sync: std::path::PathBuf,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let state = dir.path().join("state");
    let memory = dir.path().join("memory");
    let sync = dir.path().join("memory/.git/omo-sync");
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&memory)?;
    Ok(Fixture {
        _dir: dir,
        state,
        memory,
        sync,
    })
}

fn store(f: &Fixture) -> DeviceItemStore {
    create_device_item_store(DeviceItemStoreOptions {
        state_dir: f.state.clone(),
        memory_root: f.memory.clone(),
        sync_dir: f.sync.clone(),
        machine_id: Some("fixed-machine".into()),
    })
}

#[test]
fn capture_materialize_persist_and_reload_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let f = fixture()?;
    let a = store(&f);
    a.load()?;
    let ops = a
        .capture("note.md", Some("hello\n"))?
        .ok_or("first capture should produce ops")?;
    a.apply("note.md", &ops)?;
    a.mark_materialized("note.md", Some("hello\n"))?;
    a.persist()?;
    // The unchanged working text is no edit once materialised.
    assert!(a.capture("note.md", Some("hello\n"))?.is_none());
    assert_eq!(a.render("note.md", None)?.as_deref(), Some("hello\n"));
    let version = a.version("note.md")?;
    assert!(!version.is_empty());

    let b = store(&f);
    b.load()?;
    assert_eq!(b.keys(), vec!["note.md".to_string()]);
    assert_eq!(b.render("note.md", None)?.as_deref(), Some("hello\n"));
    assert_eq!(b.version("note.md")?, version);
    assert_eq!(a.peer()?, b.peer()?);
    Ok(())
}

#[test]
fn a_deleted_working_file_captures_a_tombstone_and_recreation_revives_it()
-> Result<(), Box<dyn std::error::Error>> {
    let f = fixture()?;
    let s = store(&f);
    s.load()?;
    let ops = s.capture("n.md", Some("body\n"))?.ok_or("create ops")?;
    s.apply("n.md", &ops)?;
    s.mark_materialized("n.md", Some("body\n"))?;
    let tomb = s.capture("n.md", None)?.ok_or("tombstone ops")?;
    s.apply("n.md", &tomb)?;
    s.mark_materialized("n.md", None)?;
    assert_eq!(s.render("n.md", None)?, None);
    let revived = s
        .capture("n.md", Some("body\n"))?
        .ok_or("explicit revive op")?;
    s.apply("n.md", &revived)?;
    assert_eq!(s.render("n.md", None)?.as_deref(), Some("body\n"));
    Ok(())
}

#[test]
fn use_before_load_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let f = fixture()?;
    assert!(store(&f).capture("x.md", Some("t")).is_err());
    Ok(())
}
