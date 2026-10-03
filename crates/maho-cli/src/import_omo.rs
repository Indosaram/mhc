use std::fs;
use std::io;
use std::path::{Path, PathBuf};
const ENTRIES: [&str; 5] = ["settings.json", "models.json", "keybindings.json", "auth.json", "sessions"];

pub fn import_omo(from: &Path, to: &Path, _force: bool) -> io::Result<Vec<String>> {
    if !from.is_dir() { return Err(io::Error::new(io::ErrorKind::NotFound, "omo agent directory does not exist")); }
    let source = fs::canonicalize(from)?;
    let target = absolute_target(to)?;
    if target.starts_with(&source) || source.starts_with(&target) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "source and destination must be separate directories")); }
    let mut files = Vec::new();
    for entry in ENTRIES { let path = from.join(entry); if path.exists() { collect_files(&path, &to.join(entry), &mut files)?; } }
    for entry in ENTRIES { if from.join(entry).exists() && to.join(entry).exists() { return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("destination already contains {entry}; import never overwrites existing state"))); } }
    for (source, destination) in &files {
        if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
        let mut input = fs::File::open(source)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut output = options.open(destination)?;
        io::copy(&mut input, &mut output)?;
    }
    for entry in ENTRIES { if from.join(entry).is_dir() { fs::create_dir_all(to.join(entry))?; } }
    Ok(ENTRIES.into_iter().filter(|entry| from.join(entry).exists()).map(str::to_owned).collect())
}
fn absolute_target(path: &Path) -> io::Result<PathBuf> {
    if path.exists() { return fs::canonicalize(path); }
    let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no name"))?;
    Ok(absolute_target(parent)?.join(name))
}
fn collect_files(source: &Path, target: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "import refuses symbolic links")); }
    if target.exists() && fs::symlink_metadata(target)?.file_type().is_symlink() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "import refuses destination symbolic links")); }
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
    #[test] fn refuses_overwrite_even_when_force_is_set() {
        let tmp = tempfile::tempdir().expect("tempdir"); let from = tmp.path().join("omo"); let to = tmp.path().join("maho");
        fs::create_dir(&from).expect("mkdir"); fs::create_dir(&to).expect("mkdir"); fs::write(from.join("settings.json"), b"new").expect("write"); fs::write(to.join("settings.json"), b"old").expect("write");
        assert_eq!(import_omo(&from, &to, true).expect_err("conflict").kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(to.join("settings.json")).expect("read"), b"old");
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
}
