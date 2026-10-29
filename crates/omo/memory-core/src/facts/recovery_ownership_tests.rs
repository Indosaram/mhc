use std::collections::BTreeMap as HashMap;

use pretty_assertions::assert_eq;

use crate::git::path_state::{GitIndexIdentity, GitPathState, GitWorktreeIdentity};

use crate::facts::recovery_ownership::{
    parse_porcelain_path, same_facts_owned_state, same_identity,
};

#[test]
fn test_parse_porcelain_path_variants() {
    // when
    let p1 = parse_porcelain_path(" M foo/bar.txt");
    let p2 = parse_porcelain_path("?? \"quoted/path.md\"");
    let p3 = parse_porcelain_path("R  old.txt -> new.txt");
    let p4 = parse_porcelain_path(" D deleted.txt");
    let p5 = parse_porcelain_path("D  deleted.txt");
    let p6 = parse_porcelain_path("DD conflict.txt");
    let p7 = parse_porcelain_path("ab");

    // then
    assert_eq!(p1, Some("foo/bar.txt"));
    assert_eq!(p2, Some("quoted/path.md"));
    assert_eq!(p3, Some("new.txt"));
    assert_eq!(p4, None);
    assert_eq!(p5, None);
    assert_eq!(p6, None);
    assert_eq!(p7, None);
}

#[test]
fn test_same_identity_structural_equality() {
    // given
    let id1 = GitIndexIdentity {
        mode: "100644".to_string(),
        oid: "abc1234".to_string(),
    };
    let id2 = GitIndexIdentity {
        mode: "100644".to_string(),
        oid: "abc1234".to_string(),
    };
    let id3 = GitIndexIdentity {
        mode: "100644".to_string(),
        oid: "different".to_string(),
    };

    // when
    let same = same_identity(&id1, &id2);
    let different = same_identity(&id1, &id3);

    // then
    assert!(same);
    assert!(!different);
}

#[test]
fn test_same_facts_owned_state_comparisons() {
    // given
    let mut state1 = HashMap::new();
    let mut state2 = HashMap::new();
    let sample = GitPathState {
        index: Some(GitIndexIdentity {
            mode: "100644".to_string(),
            oid: "abc1234".to_string(),
        }),
        worktree: GitWorktreeIdentity::File(crate::git::path_state::GitWorktreeFileIdentity {
            mode: 0o644,
            oid: "abc1234".to_string(),
        }),
    };
    state1.insert("path1.md".to_string(), sample.clone());
    state2.insert("path1.md".to_string(), sample);

    // when
    let equal_before = same_facts_owned_state(&state1, &state2);
    state2.insert(
        "path2.md".to_string(),
        GitPathState {
            index: None,
            worktree: GitWorktreeIdentity::Missing,
        },
    );
    let equal_after = same_facts_owned_state(&state1, &state2);

    // then
    assert!(equal_before);
    assert!(!equal_after);
}
