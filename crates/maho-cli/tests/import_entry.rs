use std::{fs, process::Command};

#[test]
fn real_import_from_refuses_repeat_without_destination_changes() {
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().expect("isolated import");
    let home = root.path().join("home");
    let source = root.path().join("source");
    fs::create_dir(&home).expect("home");
    fs::create_dir_all(source.join("sessions/project")).expect("source");
    let files = ["settings.json", "models.json", "keybindings.json", "auth.json", "sessions/project/session.jsonl"];
    for path in files { fs::write(source.join(path), b"{}\n").expect("fixture"); }
    let run = || Command::new(env!("CARGO_BIN_EXE_mhc"))
        .env_clear().env("HOME", &home).current_dir(&home)
        .arg("import-omo").arg("--from").arg(&source).output().expect("real mhc");
    let first = run();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let target = home.join(".maho/agent");
    let before: Vec<_> = files.iter().map(|path| {
        let path = target.join(path);
        (fs::read(&path).expect("copied bytes"), fs::metadata(path).expect("metadata").permissions())
    }).collect();
    assert!(before.iter().all(|(bytes, _)| bytes == b"{}\n"));
    fs::write(source.join("settings.json"), b"changed source").expect("change source");
    let repeated = run();
    assert_eq!(repeated.status.code(), Some(1));
    for (path, (bytes, permissions)) in files.iter().zip(before) {
        let after = fs::read(target.join(path)).expect("unchanged bytes");
        println!("IMPORT_HASH {path} before={:x} after={:x}", Sha256::digest(&bytes), Sha256::digest(&after));
        assert_eq!(after, bytes);
        assert_eq!(fs::metadata(target.join(path)).expect("unchanged metadata").permissions(), permissions);
    }
}

#[test]
fn repeat_real_import_never_clobbers_destination_bytes() {
    use sha2::{Digest, Sha256};
    let home = tempfile::tempdir().expect("isolated home");
    let source = home.path().join(".omo/agent");
    let target = home.path().join(".maho/agent");
    std::fs::create_dir_all(source.join("sessions/project")).expect("source directories");
    std::fs::write(source.join("settings.json"), b"{\"fixture\":1}").expect("settings");
    std::fs::write(source.join("sessions/project/a.jsonl"), b"session fixture\n").expect("session");
    let run = || std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(home.path()).env("HOME", home.path()).env_remove("__PI_INTERNAL_SPAWN")
        .args(["import-omo", "--from", source.to_str().expect("source path")])
        .output().expect("real import command");
    let first = run();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let paths = [target.join("settings.json"), target.join("sessions/project/a.jsonl")];
    let before: Vec<_> = paths.iter().map(|path| Sha256::digest(std::fs::read(path).expect("destination bytes"))).collect();
    std::fs::write(source.join("settings.json"), b"{\"fixture\":2}").expect("changed source");
    assert_eq!(run().status.code(), Some(1));
    let after: Vec<_> = paths.iter().map(|path| Sha256::digest(std::fs::read(path).expect("destination bytes"))).collect();
    assert_eq!(before, after);
    let forced = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(home.path()).env("HOME", home.path()).env_remove("__PI_INTERNAL_SPAWN")
        .args(["import-omo", "--from", source.to_str().expect("source path"), "--force"])
        .output().expect("forced import command");
    assert!(forced.status.success(), "{}", String::from_utf8_lossy(&forced.stderr));
    assert_eq!(std::fs::read(target.join("settings.json")).expect("updated settings"), b"{\"fixture\":2}");
    assert_eq!(std::fs::read(target.join("sessions/project/a.jsonl")).expect("session"), b"session fixture\n");
}
