//! Port of senpi `core/moved-session-cwd.ts` (senpi#2990).
use crate::paths::{PathInputOptions, resolve_path};
use std::collections::HashMap;

fn resolved(path: &str) -> String {
    resolve_path(path, path, &PathInputOptions::default())
}

/// Whether a session's recorded cwd is `resolved_cwd`, as recorded or where the OmO desktop moved it.
/// The moved-path walk is synchronous filesystem work, so each matcher resolves a distinct recorded
/// cwd at most once: create one per listing call, and a filter re-run on every progress tick costs
/// one walk per distinct cwd.
pub fn session_cwd_matcher(resolved_cwd: &str) -> impl FnMut(Option<&str>) -> bool {
    session_cwd_matcher_with(
        resolved_cwd,
        crate::session_manager::resolve_moved_session_path,
    )
}

pub(crate) fn session_cwd_matcher_with(
    resolved_cwd: &str,
    resolve_moved: impl Fn(&str) -> String,
) -> impl FnMut(Option<&str>) -> bool {
    let resolved_cwd = resolved_cwd.to_owned();
    let mut mapped = HashMap::<String, String>::new();
    move |cwd| {
        let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
            return false;
        };
        if resolved(cwd) == resolved_cwd {
            return true;
        }
        let target = mapped
            .entry(cwd.to_owned())
            .or_insert_with(|| resolved(&resolve_moved(cwd)));
        *target == resolved_cwd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn matches_recorded_or_moved_cwd_and_walks_each_distinct_cwd_once() {
        let walks = Cell::new(0);
        let mut matcher = session_cwd_matcher_with("/new/project", |cwd| {
            walks.set(walks.get() + 1);
            cwd.replace("/old/", "/new/")
        });
        assert!(!matcher(None));
        assert!(!matcher(Some("")));
        assert!(matcher(Some("/new/project/")));
        assert_eq!(walks.get(), 0, "an exact match needs no moved-path walk");
        assert!(matcher(Some("/old/project")));
        assert!(matcher(Some("/old/project")));
        assert!(!matcher(Some("/old/other")));
        assert!(!matcher(Some("/old/other")));
        assert_eq!(walks.get(), 2, "one walk per distinct recorded cwd");
    }

    #[test]
    fn real_resolver_follows_a_trusted_desktop_move() {
        let home = tempfile::tempdir().expect("home");
        let old = home.path().join("old");
        let new = home.path().join("new");
        crate::schedule::store::ensure_dir(&old).expect("old");
        crate::schedule::store::ensure_dir(&new).expect("new");
        crate::schedule::store::write_atomic(&old.join("omo-desktop-moved.json"), &serde_json::json!({"kind":"omo-desktop-moved","schemaVersion":1,"homeId":"h","movedTo":new,"moved":["worktrees/project"]}).to_string()).expect("breadcrumb");
        crate::schedule::store::write_atomic(&new.join("omo-desktop-home.json"), &serde_json::json!({"kind":"omo-desktop-data-home","schemaVersion":1,"appId":"com.omo.desktop","homeId":"h"}).to_string()).expect("marker");
        let here = new.join("worktrees/project");
        let mut matcher = session_cwd_matcher_with(&here.to_string_lossy(), |cwd| {
            crate::schedule::moved_paths::resolve_in_home(std::path::Path::new(cwd), home.path())
                .to_string_lossy()
                .into_owned()
        });
        assert!(matcher(Some(
            &old.join("worktrees/project").to_string_lossy()
        )));
        assert!(!matcher(Some(
            &old.join("worktrees/other").to_string_lossy()
        )));
    }
}
