use std::fs;
use std::io;
use std::path::{Path, PathBuf};
const ENTRIES: [&str; 5] = ["settings.json", "models.json", "keybindings.json", "auth.json", "sessions"];

pub fn import_omo(from: &Path, to: &Path, force: bool) -> io::Result<Vec<String>> {
    if !from.is_dir() { return Err(io::Error::new(io::ErrorKind::NotFound, "omo agent directory does not exist")); }
    let source = fs::canonicalize(from)?;
    let target = absolute_target(to)?;
    if target.starts_with(&source) || source.starts_with(&target) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "source and destination must be separate directories")); }
    let mut files = Vec::new();
    for entry in ENTRIES { let path = from.join(entry); if path.exists() { collect_files(&path, &target.join(entry), &mut files)?; } }
    if !force { for entry in ENTRIES { if from.join(entry).exists() && target.join(entry).exists() { return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("destination already contains {entry}; use --force to overwrite"))); } } }
    for (source, destination) in &files {
        if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
        let mut input = fs::File::open(source)?;
        if force && destination.exists() { fs::remove_file(destination)?; }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut output = options.open(destination)?;
        io::copy(&mut input, &mut output)?;
    }
    for entry in ENTRIES { if from.join(entry).is_dir() { fs::create_dir_all(target.join(entry))?; } }
    Ok(ENTRIES.into_iter().filter(|entry| from.join(entry).exists()).map(str::to_owned).collect())
}
fn absolute_target(path: &Path) -> io::Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "import refuses destination symbolic links")); }
    if path.exists() { return fs::canonicalize(path); }
    let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no name"))?;
    Ok(absolute_target(parent)?.join(name))
}
fn collect_files(source: &Path, target: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "import refuses symbolic links")); }
    if fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "import refuses destination symbolic links")); }
    if meta.is_dir() { for entry in fs::read_dir(source)? { let entry = entry?; collect_files(&entry.path(), &target.join(entry.file_name()), files)?; } }
    else if meta.is_file() { files.push((source.to_owned(), target.to_owned())); }
    else { return Err(io::Error::new(io::ErrorKind::InvalidInput, "import only supports regular files and directories")); }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn copies_config_and_nested_sessions_when_destination_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir_all(from.join("sessions/project")).expect("mkdir");
        fs::write(from.join("auth.json"), b"{\"opaque\":true}").expect("write"); fs::write(from.join("sessions/project/a.jsonl"), b"session\n").expect("write");
        let copied = import_omo(&from, &to, false).expect("import");
        assert_eq!(copied, vec!["auth.json", "sessions"]); assert_eq!(fs::read(to.join("sessions/project/a.jsonl")).expect("read"), b"session\n");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(fs::metadata(to.join("auth.json")).expect("metadata").permissions().mode() & 0o777, 0o600); }
    }
    #[test] fn refuses_all_writes_when_any_entry_exists() {
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir(&from).expect("mkdir"); fs::create_dir(&to).expect("mkdir"); fs::write(from.join("settings.json"), b"new").expect("write"); fs::write(from.join("models.json"), b"models").expect("write"); fs::write(to.join("settings.json"), b"old").expect("write");
        let error = import_omo(&from, &to, false).expect_err("conflict");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists); assert_eq!(fs::read(to.join("settings.json")).expect("read"), b"old"); assert!(!to.join("models.json").exists());
    }
    #[test] fn overwrites_requested_files_when_force_is_set() {
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir(&from).expect("mkdir"); fs::create_dir(&to).expect("mkdir"); fs::write(from.join("settings.json"), b"new").expect("write"); fs::write(to.join("settings.json"), b"old").expect("write");
        fs::create_dir_all(from.join("sessions/project")).expect("mkdir");
        fs::write(from.join("sessions/project/a.jsonl"), b"session\n").expect("write");
        fs::write(to.join("unrelated.json"), b"keep").expect("write");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(to.join("settings.json"), fs::Permissions::from_mode(0o644)).expect("permissions"); }
        assert_eq!(import_omo(&from, &to, true).expect("forced import"), vec!["settings.json", "sessions"]);
        assert_eq!(fs::read(to.join("settings.json")).expect("read"), b"new");
        assert_eq!(fs::read(to.join("sessions/project/a.jsonl")).expect("read"), b"session\n");
        assert_eq!(fs::read(to.join("unrelated.json")).expect("read"), b"keep");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; for path in ["settings.json", "sessions/project/a.jsonl"] { assert_eq!(fs::metadata(to.join(path)).expect("metadata").permissions().mode() & 0o777, 0o600); } }
    }
    #[test] fn repeat_import_preserves_all_destination_hashes() {
        use sha2::{Digest, Sha256};
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir_all(from.join("sessions/project")).expect("mkdir");
        fs::write(from.join("settings.json"), b"settings").expect("write");
        fs::write(from.join("sessions/project/a.jsonl"), b"session\n").expect("write");
        import_omo(&from, &to, false).expect("first import");
        let paths = [to.join("settings.json"), to.join("sessions/project/a.jsonl")];
        let before: Vec<_> = paths.iter().map(|path| Sha256::digest(fs::read(path).expect("read"))).collect();
        fs::write(from.join("settings.json"), b"changed source").expect("write");
        assert_eq!(import_omo(&from, &to, false).expect_err("repeat refused").kind(), io::ErrorKind::AlreadyExists);
        let after: Vec<_> = paths.iter().map(|path| Sha256::digest(fs::read(path).expect("read"))).collect();
        assert_eq!(before, after);
    }
    #[test] fn rejects_overlapping_directories_before_copying() {
        let tmp = tempfile::tempdir().expect("tempdir"); fs::create_dir(tmp.path().join("sessions")).expect("mkdir");
        assert_eq!(import_omo(tmp.path(), &tmp.path().join("sessions/import"), true).expect_err("overlap").kind(), io::ErrorKind::InvalidInput);
    }
    #[cfg(unix)]
    #[test] fn forced_import_rejects_destination_links_before_writing() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir(&from).expect("mkdir"); fs::create_dir(&to).expect("mkdir");
        fs::write(from.join("settings.json"), b"new").expect("write");
        let outside = tmp.path().join("outside"); fs::write(&outside, b"keep").expect("write");
        for link in [&outside, &tmp.path().join("missing")] {
            symlink(link, to.join("settings.json")).expect("symlink");
            assert_eq!(import_omo(&from, &to, true).expect_err("link refused").kind(), io::ErrorKind::InvalidInput);
            fs::remove_file(to.join("settings.json")).expect("remove link");
        }
        assert_eq!(fs::read(&outside).expect("read"), b"keep");
        assert!(!tmp.path().join("missing").exists());
        let linked_root = tmp.path().join("linked-maho"); symlink(&to, &linked_root).expect("root symlink");
        assert_eq!(import_omo(&from, &linked_root, true).expect_err("root link refused").kind(), io::ErrorKind::InvalidInput);
        assert!(!to.join("settings.json").exists());
    }
    #[test] fn forced_import_replaces_hard_links_without_changing_external_files() {
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir(&from).expect("mkdir"); fs::create_dir(&to).expect("mkdir");
        fs::write(from.join("settings.json"), b"new").expect("write");
        let outside = tmp.path().join("outside"); fs::write(&outside, b"keep").expect("write");
        fs::hard_link(&outside, to.join("settings.json")).expect("hard link");
        import_omo(&from, &to, true).expect("forced import");
        assert_eq!(fs::read(to.join("settings.json")).expect("read"), b"new");
        assert_eq!(fs::read(outside).expect("read"), b"keep");
    }
}
