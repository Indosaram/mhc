//! Guard target resolution (omo v5.1.52 `guard.ts`): built-ins and apply_patch open the resolved
//! path; suffix-classified external tools open the raw string, so it is judged where the OS lands.

#[cfg(unix)]
use crate::context::MemoryIdentityContext;
#[cfg(unix)]
use crate::guard::{MemoryGuard, MemoryGuardDecision};

#[cfg(unix)]
fn fixture() -> (tempfile::TempDir, MemoryIdentityContext, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "own");
    let foreign = paths.root.parent().unwrap().join("foreign/repo");
    std::fs::create_dir_all(&foreign).unwrap();
    let context = MemoryIdentityContext::new(
        "own".into(),
        paths,
        crate::binding::MemorySessionBinding {
            identity: "own".into(),
            repo_path_hash: "hash".into(),
            bound_at: 1.0,
        },
    );
    (dir, context, foreign)
}

#[cfg(unix)]
#[test]
fn dot_dot_after_a_link_in_own_memory_is_blocked_for_external_tools_only() {
    // Given: a link in own memory points into a foreign repo, and the raw path climbs out of it.
    let (dir, context, foreign) = fixture();
    std::fs::create_dir_all(foreign.join("subdir")).unwrap();
    std::fs::create_dir_all(foreign.join("system")).unwrap();
    std::fs::write(foreign.join("system/persona.md"), "foreign").unwrap();
    std::fs::create_dir_all(&context.identity_paths.repo).unwrap();
    std::os::unix::fs::symlink(
        foreign.join("subdir"),
        context.identity_paths.repo.join("jump"),
    )
    .unwrap();
    let raw = format!(
        "{}/jump/../system/persona.md",
        context.identity_paths.repo.display()
    );
    let input = serde_json::json!({ "path": raw });
    let mut guard = MemoryGuard::default();
    // When: an external read opens the raw string, which the OS resolves through the link.
    let external = guard
        .check(Some(&context), dir.path(), "vendor_read", &input, None, &[])
        .unwrap();
    // Then
    assert_eq!(
        external,
        MemoryGuardDecision::Block {
            reason: format!(
                "cross-identity memory access denied: vendor_read to {raw} belongs to another memory identity"
            )
        }
    );
    // When: the built-in read resolves the path lexically first, so it stays in own memory.
    let builtin = guard
        .check(Some(&context), dir.path(), "read", &input, None, &[])
        .unwrap();
    // Then
    assert_eq!(builtin, MemoryGuardDecision::Allow);
}

#[test]
fn the_memory_tool_teaches_time_boxed_content_with_an_expires_instant() {
    let line = crate::tool_metadata::MEMORY_TOOL_DESCRIPTION
        .lines()
        .find(|line| line.starts_with("Time-boxed content:"))
        .expect("time-boxed content line");
    assert!(line.contains("expires: <UTC date or ISO instant with Z/offset>"));
}
