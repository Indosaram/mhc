//! The launch profile with paths the OmO desktop moved resolved to where they live now
//! (senpi `rpc/session-registry-moved-path.ts`, senpi#2898), taken before the reservation so an old
//! spelling of a moved file is the same session. A moved session file that is gone is reported where it
//! should be, never re-created there.
use crate::session_registry::{RpcSessionLaunchProfile, RpcSessionRegistryError};
use maho_core::session_manager::resolve_moved_session_path;
use std::path::Path;

pub fn resolve_moved_profile(
    requested: &RpcSessionLaunchProfile,
) -> Result<RpcSessionLaunchProfile, RpcSessionRegistryError> {
    resolve_moved_profile_with(requested, resolve_moved_session_path)
}

fn resolve_moved_profile_with(
    requested: &RpcSessionLaunchProfile,
    resolve: impl Fn(&str) -> String,
) -> Result<RpcSessionLaunchProfile, RpcSessionRegistryError> {
    let mut profile = requested.clone();
    profile.runtime.cwd = resolve(&requested.runtime.cwd);
    if let Some(session_path) = requested
        .session_path
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        profile.session_path = Some(resolve(session_path));
    }
    if profile.session_path != requested.session_path
        && let Some(moved) = profile.session_path.as_deref()
        && !Path::new(moved).exists()
    {
        return Err(RpcSessionRegistryError::new(
            "open_failed",
            Some(format!("moved session file does not exist: {moved}").as_str()),
            None,
        ));
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested(path: &str) -> RpcSessionLaunchProfile {
        let mut profile = RpcSessionLaunchProfile {
            session_path: Some(path.to_owned()),
            ..Default::default()
        };
        profile.runtime.cwd = "/old/w".into();
        profile
    }

    #[test]
    fn a_moved_file_that_exists_is_opened_where_it_lives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "").expect("write");
        let target = file.to_string_lossy().into_owned();
        let profile = resolve_moved_profile_with(&requested("/old/s.jsonl"), |path| {
            if path == "/old/s.jsonl" {
                target.clone()
            } else {
                path.replace("/old", "/new")
            }
        })
        .expect("resolved");
        assert_eq!(profile.session_path.as_deref(), Some(target.as_str()));
        assert_eq!(profile.runtime.cwd, "/new/w");
    }

    #[test]
    fn a_moved_file_that_is_gone_is_refused() {
        let error = resolve_moved_profile_with(&requested("/old/s.jsonl"), |path| {
            path.replace("/old", "/gone-x")
        })
        .expect_err("refused");
        assert_eq!(
            error.to_string(),
            "open_failed: moved session file does not exist: /gone-x/s.jsonl"
        );
    }

    #[test]
    fn an_unmoved_missing_file_is_left_to_the_open() {
        let profile = resolve_moved_profile_with(&requested("/nowhere/s.jsonl"), str::to_owned)
            .expect("unchanged");
        assert_eq!(profile.session_path.as_deref(), Some("/nowhere/s.jsonl"));
    }
}
