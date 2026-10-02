use maho_cli::experimental::mini::{presentation::{presentation_paths, continued_session}, shared::protocol::SessionSummary};
#[test]
fn continuation_selects_latest_matching_cwd_and_last_timestamp_tie() {
    let sessions: Vec<_> = [("old", "/cwd", 1.0), ("other", "/other", 100.0), ("first", "/cwd", 2.0), ("last", "/cwd", 2.0)].into_iter().map(|(id, cwd, created_at)| SessionSummary { id: id.to_owned(), cwd: cwd.to_owned(), created_at, path: format!("/{id}") }).collect();
    assert_eq!(continued_session(&sessions, "/cwd"), Some("last"));
    assert_eq!(continued_session(&sessions, "/missing"), None);
    assert_eq!(continued_session(&[], "/cwd"), None);
}
#[test]
fn presentation_paths_keep_sessions_separate_from_regular_archive() {
    let paths = presentation_paths(std::path::Path::new("/agent"));
    assert_eq!(paths.root, std::path::Path::new("/agent/experimental"));
    assert_eq!(paths.socket, paths.root.join("mini.sock"));
    assert_eq!(paths.sessions_root, paths.root.join("mini-sessions"));
}
