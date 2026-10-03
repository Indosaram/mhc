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
