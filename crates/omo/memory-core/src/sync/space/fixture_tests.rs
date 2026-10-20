use super::*;
use std::collections::BTreeMap;

#[test]
fn upstream_remote_url_table_is_canonical() {
    // Given
    let cases = [
        ("git@github.com:/a/b.git", "github.com/a/b"),
        ("https://github.com/a/b.GIT/", "github.com/a/b"),
        ("https://github.com/a/b/", "github.com/a/b"),
        ("https://github.com/a/b.git/", "github.com/a/b"),
        ("https://user:pass@github.com/a/b.git", "github.com/a/b"),
        ("http://github.com:80/a/b", "github.com/a/b"),
        (
            "git@gitlab.com:group/sub/proj.git",
            "gitlab.com/group/sub/proj",
        ),
        ("https://github.com/A/B", "github.com/A/B"),
        ("ssh://git@github.com:2222/a/b", "github.com:2222/a/b"),
    ];
    for (input, expected) in cases {
        // When
        let actual = canonical_remote_url(input).expect("remote");
        // Then
        assert_eq!(actual, expected, "{input}");
    }
}

#[test]
fn remote_selection_prefers_origin_then_alphabetical_usable_remote() {
    // Given
    let remotes = [
        Remote {
            name: "upstream".into(),
            url: "git@github.com:x/y.git".into(),
        },
        Remote {
            name: "alice".into(),
            url: "git@github.com:a/b.git".into(),
        },
        Remote {
            name: "origin".into(),
            url: "git@github.com:o/p.git".into(),
        },
    ];
    // When / Then
    assert_eq!(
        repo_binding(&remotes).as_deref(),
        Some("repo:github.com/o/p")
    );
    assert_eq!(
        repo_binding(&remotes[..2]).as_deref(),
        Some("repo:github.com/a/b")
    );
    assert_eq!(repo_binding(&[]), None);
    assert_eq!(
        repo_binding(&[Remote {
            name: "origin".into(),
            url: "https://[bad".into()
        }]),
        None
    );
}

#[test]
fn binding_parsing_rejects_empty_unknown_and_malformed_entropy() {
    // Given
    let invalid = ["bogus:x", "repo:", "named:", "local:xyz", ""];
    // When / Then
    for input in invalid {
        assert_eq!(parse_binding(input), None);
    }
    for input in [
        "zz".to_owned(),
        "ab".repeat(15),
        "AB".repeat(16),
        "0".repeat(33),
    ] {
        assert!(local_binding(&input).is_err());
    }
    assert_eq!(
        parse_binding("named:Dori"),
        Some((BindingKind::Named, "Dori"))
    );
    assert_eq!(
        parse_binding("repo:github.com/a/b"),
        Some((BindingKind::Repo, "github.com/a/b"))
    );
    assert_eq!(
        synced_agent_dir_name("88c3c57d6699688f3cdd91b31b7e9f4d"),
        "synced-88c3c57d"
    );
    assert_ne!(
        space_id_for_binding("repo:github.com/a/b"),
        space_id_for_binding("repo:github.com/a/c")
    );
}

#[test]
fn auto_profiles_fall_through_and_corrupt_local_binding_is_reminted() {
    // Given
    let remotes = [Remote {
        name: "origin".into(),
        url: "git@github.com:a/b.git".into(),
    }];
    for profile in [None, Some(""), Some("auto")] {
        let mut store = BTreeMap::new();
        // When
        let result = resolve_space(
            &mut store,
            ResolveSpaceOptions {
                profile,
                remotes: &remotes,
                random_hex: None,
            },
        )
        .expect("resolve");
        // Then
        assert_eq!(result.binding_key, "repo:github.com/a/b");
    }
    let mut store = BTreeMap::from([(SYNC_BINDING_CONFIG_KEY.into(), "garbage".into())]);
    let entropy = || "f".repeat(32);
    let result = resolve_space(
        &mut store,
        ResolveSpaceOptions {
            random_hex: Some(&entropy),
            ..Default::default()
        },
    )
    .expect("resolve");
    assert_eq!(result.binding_key, format!("local:{}", "f".repeat(32)));
    assert_eq!(store[SYNC_BINDING_CONFIG_KEY], result.binding_key);
}

#[test]
fn failed_config_writes_propagate_instead_of_returning_unpersisted_identity() {
    struct RefusingStore;
    impl SpaceConfigStore for RefusingStore {
        fn get(&self, _: &str) -> Result<Option<String>, SpaceError> {
            Ok(None)
        }
        fn set(&mut self, key: &str, _: &str) -> Result<(), SpaceError> {
            Err(SpaceError::GitWrite(key.into()))
        }
    }
    // Given
    let mut store = RefusingStore;
    // When
    let result = resolve_space(
        &mut store,
        ResolveSpaceOptions {
            profile: Some("Dori"),
            ..Default::default()
        },
    );
    // Then
    assert!(matches!(result, Err(SpaceError::GitWrite(key)) if key == SYNC_SPACE_CONFIG_KEY));
}

#[test]
fn absorbed_manifest_cannot_be_rejoined_elsewhere_or_into_itself() {
    // Given
    let a = create_manifest_doc(1).expect("manifest");
    let b = create_manifest_doc(2).expect("manifest");
    let c = create_manifest_doc(3).expect("manifest");
    manifest_add_binding(&b, "named:y").expect("binding");
    join_spaces(&a, "aaaa", &b, "bbbb").expect("join");
    // When
    let result = join_spaces(&c, "0000", &b, "bbbb").expect("rejoin");
    // Then
    assert!(!result.changed);
    assert_eq!(result.target_id, "aaaa");
    assert!(manifest_bindings(&c).is_empty());
    assert_eq!(manifest_absorbed_into(&b).as_deref(), Some("aaaa"));
    assert!(
        !join_spaces(&a, "aaaa", &a, "aaaa")
            .expect("self join")
            .changed
    );
    assert_eq!(manifest_absorbed_into(&a), None);
    assert_eq!(join_direction("aaaa", "aaaa"), None);
}

#[test]
fn upstream_side_file_names_preserve_targets_and_reject_lookalikes() {
    // Given
    let hits = [
        ("notes/a.sync-conflict-20261015-mini.md", "notes/a.md"),
        ("x.sync-conflict-20261015-host", "x"),
        (
            "archive.tar.sync-conflict-20261015-mac.gz",
            "archive.tar.gz",
        ),
        ("a.sync-conflict-20261015-.md", "a.md"),
        (
            "people/hojin/card.sync-conflict-20260101-jobdori2.md",
            "people/hojin/card.md",
        ),
    ];
    // When / Then
    for (input, target) in hits {
        assert_eq!(
            parse_sync_conflict_name(input).expect("side").target,
            target
        );
    }
    for input in [
        "a.md",
        "a.sync-conflict-2026101-x.md",
        "a.sync-conflict-2026-10-15-x.md",
        "a.sync-conflict-20261015-x y.md",
        "a.sync-conflict-20261015-x.md.bak",
        ".sync-conflict-20261015-x.md",
    ] {
        assert_eq!(parse_sync_conflict_name(input), None, "{input}");
    }
    let parsed = parse_sync_conflict_name("a.sync-conflict-20261015-mini.md").expect("side");
    assert_eq!(
        (parsed.day.as_str(), parsed.label.as_str()),
        ("20261015", "mini")
    );
}
