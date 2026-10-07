use isolation_core::{resolve_candidates, BackendKind, IsolationError};

fn kinds(platform: &str) -> Vec<BackendKind> {
    resolve_candidates(platform, None).candidates
}

#[test]
fn orders_candidates_on_darwin() {
    assert_eq!(
        kinds("darwin"),
        vec![BackendKind::Apfs, BackendKind::Zfs, BackendKind::Rcopy]
    );
    assert_eq!(
        resolve_candidates("darwin", None).candidates,
        vec![BackendKind::Apfs, BackendKind::Zfs, BackendKind::Rcopy]
    );
}

#[test]
fn orders_candidates_on_linux() {
    assert_eq!(
        kinds("linux"),
        vec![
            BackendKind::Btrfs,
            BackendKind::Zfs,
            BackendKind::Reflink,
            BackendKind::Overlayfs,
            BackendKind::Rcopy,
        ]
    );
}

#[test]
fn orders_candidates_on_win32() {
    assert_eq!(
        kinds("win32"),
        vec![BackendKind::BlockClone, BackendKind::Rcopy]
    );
}

#[test]
fn orders_candidates_on_freebsd() {
    assert_eq!(kinds("freebsd"), vec![BackendKind::Rcopy]);
}

#[test]
fn puts_preferred_first_without_duplicating_it() {
    assert_eq!(
        resolve_candidates("linux", Some(BackendKind::Zfs)).candidates,
        vec![
            BackendKind::Zfs,
            BackendKind::Btrfs,
            BackendKind::Reflink,
            BackendKind::Overlayfs,
            BackendKind::Rcopy,
        ]
    );
    assert_eq!(
        resolve_candidates("darwin", Some(BackendKind::BlockClone)).candidates,
        vec![
            BackendKind::BlockClone,
            BackendKind::Apfs,
            BackendKind::Zfs,
            BackendKind::Rcopy,
        ]
    );
}

#[test]
fn distinguishes_unavailable_from_other_failures() {
    let unavailable = IsolationError::unavailable("not supported");
    assert_eq!(unavailable.code(), Some("isolation_unavailable"));
    assert!(unavailable.is_unavailable());
    assert!(!IsolationError::other("I/O failure").is_unavailable());
}
