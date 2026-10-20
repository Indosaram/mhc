//! Synchronous desktop move resolution used by session opens and schedule delivery.
use std::{fs, io::Read, path::{Component, Path, PathBuf}};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Breadcrumb {
    kind: String,
    schema_version: u64,
    moved_to: PathBuf,
    home_id: String,
    moved: Vec<String>,
}

fn trusted_metadata(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == nix::unistd::Uid::current().as_raw() && metadata.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn trusted_json(path: &Path) -> Option<serde_json::Value> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if !fs::symlink_metadata(path).ok()?.is_file() { return None; }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 65_536 || !trusted_metadata(&metadata) { return None; }
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 65_536 { return None; }
    serde_json::from_slice(&bytes).ok()
}

fn same_component(left: Component<'_>, right: Component<'_>) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        left.as_os_str().to_string_lossy().to_lowercase() == right.as_os_str().to_string_lossy().to_lowercase()
    } else {
        left == right
    }
}

fn below(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    for part in root.components() {
        if !same_component(components.next()?, part) { return None; }
    }
    Some(components.collect())
}

fn canonical_missing(path: &Path) -> Option<PathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    let mut ancestor = absolute.as_path();
    let mut tail = Vec::new();
    while !ancestor.try_exists().ok()? {
        tail.push(ancestor.file_name()?.to_owned());
        ancestor = ancestor.parent()?;
    }
    let mut result = fs::canonicalize(ancestor).ok()?;
    result.extend(tail.into_iter().rev());
    Some(result)
}

fn moved_once(path: &Path, home: &Path) -> Option<PathBuf> {
    let canonical = canonical_missing(path).or_else(|| std::path::absolute(path).ok())?;
    let real_home = fs::canonicalize(home).ok();
    for root in canonical.parent()?.ancestors() {
        let Some(raw) = trusted_json(&root.join("omo-desktop-moved.json")) else { continue };
        let Ok(breadcrumb) = serde_json::from_value::<Breadcrumb>(raw) else { continue };
        if breadcrumb.kind != "omo-desktop-moved" || breadcrumb.schema_version != 1 || breadcrumb.home_id.is_empty()
            || !breadcrumb.moved_to.is_absolute()
            || breadcrumb.moved_to.components().collect::<PathBuf>().as_os_str() != breadcrumb.moved_to.as_os_str()
            || breadcrumb.moved_to.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir))
            || below(&breadcrumb.moved_to, root).is_some()
            || !(below(&breadcrumb.moved_to, home).is_some() || real_home.as_ref().is_some_and(|real| below(&breadcrumb.moved_to, real).is_some()))
        { continue; }
        let Some(remainder) = below(&canonical, root) else { continue };
        let prefixes: Option<Vec<PathBuf>> = breadcrumb.moved.iter().map(|prefix| {
            let segments: Vec<_> = prefix.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
            if prefix.starts_with(['/', '\\']) || segments.is_empty()
                || segments.iter().any(|part| matches!(*part, "." | "..") || part.contains(':'))
            { return None; }
            Some(segments.iter().collect())
        }).collect();
        let Some(prefixes) = prefixes else { continue };
        let Some(prefix) = prefixes.iter().find(|prefix| below(&remainder, prefix).is_some()) else { continue };
        let legacy = [home.join(".t3"), home.join(".omo-app")].iter().any(|legacy| below(root, legacy).is_some_and(|tail| tail.as_os_str().is_empty()));
        let metadata = if legacy { fs::metadata(root) } else { fs::symlink_metadata(root) };
        let Ok(metadata) = metadata else { continue };
        if !metadata.is_dir() || !trusted_metadata(&metadata) { continue; }
        let Some(marker) = trusted_json(&breadcrumb.moved_to.join("omo-desktop-home.json")) else { continue };
        if marker["kind"] != "omo-desktop-data-home" || marker["appId"] != "com.omo.desktop"
            || marker["schemaVersion"] != 1 || marker["homeId"] != breadcrumb.home_id
        { continue; }
        if prefix.components().any(|part| part.as_os_str() == "worktrees") {
            match root.join(prefix).join(".git").try_exists() {
                Ok(false) => {},
                Ok(true) | Err(_) => continue,
            }
        }
        return Some(breadcrumb.moved_to.join(remainder));
    }
    None
}

pub(crate) fn resolve_in_home(path: &Path, home: &Path) -> PathBuf {
    let mut mapped = path.to_path_buf();
    for _ in 0..3 {
        let Some(next) = moved_once(&mapped, home) else { break };
        mapped = next;
    }
    mapped
}

pub(crate) fn resolve(path: &str) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    home.map_or_else(|| path.to_owned(), |home| resolve_in_home(Path::new(path), Path::new(&home)).to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(home: &Path, old: &str, new: &str, id: &str) -> (PathBuf, PathBuf) {
        let old = home.join(old);
        let new = home.join(new);
        super::super::store::ensure_dir(&old).expect("old root");
        super::super::store::ensure_dir(&new).expect("new root");
        super::super::store::write_atomic(&old.join("omo-desktop-moved.json"), &serde_json::json!({
            "kind":"omo-desktop-moved", "schemaVersion":1, "homeId":id,
            "movedTo":new, "moved":["sessions", "worktrees/project"]
        }).to_string()).expect("breadcrumb");
        super::super::store::write_atomic(&new.join("omo-desktop-home.json"), &serde_json::json!({
            "kind":"omo-desktop-data-home", "schemaVersion":1, "appId":"com.omo.desktop", "homeId":id
        }).to_string()).expect("marker");
        (old, new)
    }

    #[test]
    fn moved_session_and_cwd_follow_bound_home_without_requiring_old_files() {
        // Given
        let home = tempfile::tempdir().expect("home");
        let (old, new) = fixture(home.path(), "old", "new", "home-a");
        // When
        let session = resolve_in_home(&old.join("sessions/a.jsonl"), home.path());
        let cwd = resolve_in_home(&old.join("worktrees/project/src"), home.path());
        // Then
        assert_eq!(session, new.join("sessions/a.jsonl"));
        assert_eq!(cwd, new.join("worktrees/project/src"));
        assert_eq!(resolve_in_home(&old.join("sessions-other/a"), home.path()), old.join("sessions-other/a"));
    }

    #[test]
    fn mismatched_home_marker_does_not_redirect() {
        // Given
        let home = tempfile::tempdir().expect("home");
        let (old, new) = fixture(home.path(), "old", "new", "home-a");
        super::super::store::write_atomic(&new.join("omo-desktop-home.json"), &serde_json::json!({
            "kind":"omo-desktop-data-home", "schemaVersion":1, "appId":"com.omo.desktop", "homeId":"another-home"
        }).to_string()).expect("marker");
        let path = old.join("sessions/a.jsonl");
        // When
        let resolved = resolve_in_home(&path, home.path());
        // Then
        assert_eq!(resolved, path);
    }

    #[test]
    fn breadcrumb_cannot_redirect_outside_home() {
        // Given
        let home = tempfile::tempdir().expect("home");
        let outside = tempfile::tempdir().expect("outside");
        let (old, _) = fixture(home.path(), "old", "new", "home-a");
        super::super::store::write_atomic(&old.join("omo-desktop-moved.json"), &serde_json::json!({
            "kind":"omo-desktop-moved", "schemaVersion":1, "homeId":"home-a",
            "movedTo":outside.path(), "moved":["sessions"]
        }).to_string()).expect("breadcrumb");
        let path = old.join("sessions/a.jsonl");
        // When
        let resolved = resolve_in_home(&path, home.path());
        // Then
        assert_eq!(resolved, path);
    }

    #[test]
    fn recreated_worktree_is_not_redirected() {
        // Given
        let home = tempfile::tempdir().expect("home");
        let (old, _) = fixture(home.path(), "old", "new", "home-a");
        fs::create_dir_all(old.join("worktrees/project/.git")).expect("recreated worktree");
        let path = old.join("worktrees/project/src");
        // When
        let resolved = resolve_in_home(&path, home.path());
        // Then
        assert_eq!(resolved, path);
    }

    #[test]
    fn chained_moves_follow_at_most_three_hops() {
        // Given
        let home = tempfile::tempdir().expect("home");
        let (old, _) = fixture(home.path(), "old", "second", "home-a");
        fixture(home.path(), "second", "third", "home-a");
        fixture(home.path(), "third", "fourth", "home-a");
        fixture(home.path(), "fourth", "fifth", "home-a");
        // When
        let resolved = resolve_in_home(&old.join("sessions/a.jsonl"), home.path());
        // Then
        assert_eq!(resolved, home.path().join("fourth/sessions/a.jsonl"));
    }

    #[test]
    fn opening_old_session_path_resolves_header_cwd_without_rewriting_header() {
        // Given
        let home = crate::config::home_dir_path();
        let root = tempfile::tempdir_in(&home).expect("fixture under home");
        let (old, new) = fixture(root.path(), "old", "new", "session-home");
        let file = new.join("sessions/a.jsonl");
        let original_cwd = old.join("worktrees/project");
        let header = serde_json::json!({"type":"session", "version":3, "id":"moved-session",
            "timestamp":"2026-10-20T00:00:00.000Z", "cwd":original_cwd});
        let source = format!("{header}\n");
        super::super::store::write_atomic(&file, &source).expect("session");
        // When
        let manager = crate::session_manager::SessionManager::open(
            &old.join("sessions/a.jsonl").to_string_lossy(), None, None, None,
        ).expect("moved session opens");
        // Then
        assert_eq!(manager.session_file(), Some(file.to_string_lossy().as_ref()));
        assert_eq!(manager.cwd(), new.join("worktrees/project").to_string_lossy());
        assert_eq!(fs::read_to_string(file).expect("session unchanged"), source);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_breadcrumb_and_shared_root_are_not_trusted() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        // Given
        let home = tempfile::tempdir().expect("home");
        let (old, _) = fixture(home.path(), "old", "new", "home-a");
        fs::rename(old.join("omo-desktop-moved.json"), old.join("saved.json")).expect("save");
        symlink(old.join("saved.json"), old.join("omo-desktop-moved.json")).expect("symlink");
        let path = old.join("sessions/a.jsonl");
        // When
        let resolved = resolve_in_home(&path, home.path());
        // Then
        assert_eq!(resolved, path);
        fs::remove_file(old.join("omo-desktop-moved.json")).expect("unlink");
        fs::rename(old.join("saved.json"), old.join("omo-desktop-moved.json")).expect("restore");
        fs::set_permissions(&old, fs::Permissions::from_mode(0o777)).expect("shared root");
        assert_eq!(resolve_in_home(&path, home.path()), path);
    }
}
