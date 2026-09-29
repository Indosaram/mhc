use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::*;
use crate::identity::layout::{AGENTS_DIRNAME, MEMORY_ROOT_ENV_VAR, default_memory_root};
use crate::support::sha256::sha256_hex;

fn env_with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn expected_hash(input: &str) -> String {
    sha256_hex(input.as_bytes()).chars().take(8).collect()
}

fn assert_confined(root: &Path, candidate: &Path) {
    assert!(
        candidate.starts_with(root),
        "{candidate:?} escaped {root:?}"
    );
    for component in candidate.components() {
        assert!(
            !matches!(component, Component::ParentDir),
            "{candidate:?} contains a parent component"
        );
    }
}

#[test]
fn test_short_hash_when_hashed_then_it_is_the_first_eight_hex_chars_of_sha256() {
    let hash = short_hash("backend-lead");
    assert_eq!(hash, expected_hash("backend-lead"));
    assert_eq!(hash.len(), 8);
    assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
}

#[test]
fn test_sanitize_to_slug_when_input_is_already_safe_then_it_is_unchanged() {
    assert_eq!(sanitize_to_slug("backend-lead"), "backend-lead");
    assert_eq!(sanitize_to_slug("a1"), "a1");
}

#[test]
fn test_sanitize_to_slug_when_input_is_unicode_then_it_folds_to_ascii_or_dashes() {
    assert_eq!(sanitize_to_slug("élan"), "elan");
    assert_eq!(sanitize_to_slug("ＥＶＩＬ"), "evil");
    assert_eq!(sanitize_to_slug("e\u{202e}vil"), "e-vil");
    assert_eq!(sanitize_to_slug("漢字"), FALLBACK_SLUG);
}

#[test]
fn test_sanitize_to_slug_when_input_is_separator_heavy_then_dashes_collapse_and_trim() {
    assert_eq!(sanitize_to_slug(".."), FALLBACK_SLUG);
    assert_eq!(sanitize_to_slug(" -x- "), "x");
    assert_eq!(sanitize_to_slug("a\0b"), "a-b");
    assert_eq!(sanitize_to_slug("a//b\\\\c"), "a-b-c");
}

#[test]
fn test_sanitize_to_slug_when_input_is_overlong_then_it_caps_without_a_trailing_dash() {
    let input = format!("{}-{}", "a".repeat(39), "b".repeat(20));
    let slug = sanitize_to_slug(&input);
    assert!(slug.len() <= MAX_SLUG_LENGTH);
    assert!(!slug.ends_with('-'));
    assert_eq!(slug, "a".repeat(39));
}

#[test]
fn test_resolve_memory_identity_when_cwd_repeats_then_the_auto_id_is_deterministic() {
    let first = resolve_memory_identity(Some("auto"), Path::new("/repo/alpha"), &BTreeMap::new())
        .expect("auto identity");
    for value in [Some("auto"), None, Some(""), Some("   ")] {
        let other =
            resolve_memory_identity(value, Path::new("/repo/alpha"), &BTreeMap::new()).expect("id");
        assert_eq!(other.id, first.id);
        assert_eq!(other.paths, first.paths);
    }
}

#[test]
fn test_resolve_memory_identity_when_projects_share_a_basename_then_slugs_match_but_ids_differ() {
    let one =
        resolve_memory_identity(Some("auto"), Path::new("/repo/alpha"), &BTreeMap::new()).unwrap();
    let two =
        resolve_memory_identity(Some("auto"), Path::new("/other/alpha"), &BTreeMap::new()).unwrap();
    assert_eq!(one.safe_slug, "alpha");
    assert_eq!(two.safe_slug, "alpha");
    assert_ne!(one.id, two.id);
}

#[test]
fn test_resolve_memory_identity_when_cwd_spelling_varies_then_normalization_yields_one_id() {
    let clean =
        resolve_memory_identity(Some("auto"), Path::new("/repo/alpha"), &BTreeMap::new()).unwrap();
    let trailing =
        resolve_memory_identity(Some("auto"), Path::new("/repo/alpha/"), &BTreeMap::new()).unwrap();
    let dotted =
        resolve_memory_identity(Some("auto"), Path::new("/repo/./alpha"), &BTreeMap::new())
            .unwrap();
    assert_eq!(trailing.id, clean.id);
    assert_eq!(dotted.id, clean.id);
}

#[test]
fn test_resolve_memory_identity_when_auto_then_the_id_is_slug_plus_sha256_of_the_full_path() {
    let identity =
        resolve_memory_identity(Some("auto"), Path::new("/x/My Proj"), &BTreeMap::new()).unwrap();
    assert_eq!(identity.safe_slug, "my-proj");
    assert_eq!(
        identity.id,
        format!("my-proj-{}", expected_hash("/x/My Proj"))
    );
}

#[test]
fn test_resolve_memory_identity_when_no_override_then_paths_live_under_the_default_root() {
    let identity =
        resolve_memory_identity(Some("auto"), Path::new("/repo/alpha"), &BTreeMap::new()).unwrap();
    let expected_root = default_memory_root()
        .join(AGENTS_DIRNAME)
        .join(&identity.id);
    assert_eq!(identity.paths.root, expected_root);
    assert_eq!(identity.paths.repo, expected_root.join("repo"));
}

#[test]
fn test_resolve_memory_identity_when_explicit_then_the_directory_is_slug_plus_hash() {
    let identity = resolve_memory_identity(
        Some("backend-lead"),
        Path::new("/repo/alpha"),
        &BTreeMap::new(),
    )
    .unwrap();
    let expected_id = format!("backend-lead-{}", expected_hash("backend-lead"));
    assert_eq!(identity.safe_slug, "backend-lead");
    assert_eq!(identity.id, expected_id);
    assert_eq!(
        identity.paths.root,
        default_memory_root()
            .join(AGENTS_DIRNAME)
            .join(&expected_id)
    );
}

#[test]
fn test_resolve_memory_identity_when_explicit_has_whitespace_then_it_matches_the_trimmed_form() {
    let padded = resolve_memory_identity(
        Some("  backend-lead  "),
        Path::new("/repo/alpha"),
        &BTreeMap::new(),
    )
    .unwrap();
    let plain = resolve_memory_identity(
        Some("backend-lead"),
        Path::new("/repo/alpha"),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(padded.id, plain.id);
}

#[test]
fn test_resolve_memory_identity_when_value_is_auto_cased_then_only_lowercase_triggers_auto() {
    let keyword =
        resolve_memory_identity(Some("auto"), Path::new("/repo/alpha"), &BTreeMap::new()).unwrap();
    let named =
        resolve_memory_identity(Some("Auto"), Path::new("/repo/alpha"), &BTreeMap::new()).unwrap();
    assert_eq!(
        keyword.id,
        format!("alpha-{}", expected_hash("/repo/alpha"))
    );
    assert_eq!(named.safe_slug, "auto");
    assert_eq!(named.id, format!("auto-{}", expected_hash("Auto")));
    assert_ne!(named.id, keyword.id);
}

#[test]
fn test_resolve_memory_identity_when_root_is_overridden_then_paths_honor_it_and_stay_confined() {
    let override_root = std::env::temp_dir().join("qa-memory-home");
    let env = env_with(&[(MEMORY_ROOT_ENV_VAR, override_root.to_str().unwrap())]);
    let identity =
        resolve_memory_identity(Some("../evil"), Path::new("/repo/alpha"), &env).unwrap();
    let expected_root = override_root
        .join(AGENTS_DIRNAME)
        .join(format!("evil-{}", expected_hash("../evil")));
    assert_eq!(identity.paths.root, expected_root);
    assert_eq!(identity.paths.repo, expected_root.join("repo"));
    assert_eq!(
        identity.paths.push_queue,
        expected_root.join("runtime/push-queue")
    );
}

#[test]
fn test_resolve_memory_identity_when_root_override_is_relative_then_it_uses_the_cwd_argument() {
    let env = env_with(&[(MEMORY_ROOT_ENV_VAR, "qa-home")]);
    let identity = resolve_memory_identity(Some("auto"), Path::new("/work/proj"), &env).unwrap();
    assert_eq!(
        identity.paths.root,
        PathBuf::from("/work/proj/qa-home/agents").join(&identity.id)
    );
}

#[test]
fn test_resolve_memory_identity_when_cwd_is_blank_then_it_is_rejected() {
    assert!(resolve_memory_identity(Some("auto"), Path::new(""), &BTreeMap::new()).is_err());
    assert!(resolve_memory_identity(Some("auto"), Path::new("   "), &BTreeMap::new()).is_err());
}

#[test]
fn test_resolve_memory_identity_when_ids_are_hostile_then_every_path_stays_confined() {
    let override_root = std::env::temp_dir().join("qa-memory-home");
    let env = env_with(&[(MEMORY_ROOT_ENV_VAR, override_root.to_str().unwrap())]);
    let hostile = [
        "../evil",
        "../../etc",
        "..\\..\\windows",
        "/etc/passwd",
        "a/b/c",
        "a\0b",
        "\0",
        "..",
        ".",
        "...",
        "e\u{202e}vil",
        "ＥＶＩＬ",
        "漢字",
        " -x- ",
        "élan",
        "~/escape",
        "$HOME/evil",
    ];
    for input in hostile {
        let identity =
            resolve_memory_identity(Some(input), Path::new("/repo/alpha"), &env).unwrap();
        assert!(!identity.id.contains(".."), "id escaped: {}", identity.id);
        assert!(!identity.id.contains('/'), "id escaped: {}", identity.id);
        assert!(!identity.id.contains('\\'), "id escaped: {}", identity.id);
        assert!(!identity.id.contains('\0'), "id escaped: {}", identity.id);
        assert!(identity.id.len() <= MAX_SLUG_LENGTH + SHORT_HASH_LENGTH + 1);
        for candidate in [
            &identity.paths.root,
            &identity.paths.repo,
            &identity.paths.runtime,
            &identity.paths.locks,
            &identity.paths.facts_queue,
        ] {
            assert_confined(&override_root, candidate);
        }
    }
}

#[test]
fn test_resolve_memory_identity_when_hostile_and_honest_slugs_collide_then_hash_suffixes_differ() {
    let override_root = std::env::temp_dir().join("qa-memory-home");
    let env = env_with(&[(MEMORY_ROOT_ENV_VAR, override_root.to_str().unwrap())]);
    let hostile = resolve_memory_identity(Some("../evil"), Path::new("/repo/alpha"), &env).unwrap();
    let honest = resolve_memory_identity(Some("evil"), Path::new("/repo/alpha"), &env).unwrap();
    assert_eq!(hostile.safe_slug, "evil");
    assert_eq!(honest.safe_slug, "evil");
    assert_ne!(hostile.id, honest.id);
    assert_ne!(hostile.paths.root, honest.paths.root);
}

#[test]
fn test_is_auto_agent_value_when_value_missing_or_blank_then_it_is_auto() {
    assert!(is_auto_agent_value(None));
    assert!(is_auto_agent_value(Some("")));
    assert!(is_auto_agent_value(Some("   ")));
    assert!(is_auto_agent_value(Some("auto")));
    assert!(!is_auto_agent_value(Some("Auto")));
    assert!(!is_auto_agent_value(Some("backend-lead")));
}
