use maho_ext_config_reload::{protocol::*, reload_deferral::ReloadVetoDeferral};
use serde_json::json;
#[test]
fn accepts_valid_lifecycle_payloads() { assert!(is_config_watch_validation(&json!({"ok":true}))); assert!(is_config_watch_changed(&json!({"registrationId":"r","paths":["p"],"deferred":true}))); assert!(is_config_watch_reloaded(&json!({"registrationId":"r","paths":[]}))); assert!(is_config_watch_rejected(&json!({"registrationId":"r","paths":[],"errors":["bad"]}))); }
#[test]
fn rejects_malformed_payloads() { assert!(parse_config_watch_registration(&json!({"displayName":"d","targets":[]})).is_none()); assert!(!is_config_watch_validation(&json!({"ok":false,"errors":[1]}))); assert!(!is_config_watch_changed(&json!({"registrationId":"r","paths":"p","deferred":false}))); }
#[test]
fn parses_watch_target_with_filters() { let target = parse_config_watch_target(&json!({"path":"p","kind":"dir","filterGlobs":["*.json"]})).unwrap(); assert_eq!(target.kind, ConfigWatchTargetKind::Dir); assert_eq!(target.filter_globs, Some(vec!["*.json".into()])); }
#[test]
fn root_anchored_filter_matches_only_immediate_child() { let filters = vec!["/.omo".into()]; assert!(matches_config_watch_filter(".omo", &filters)); assert!(!matches_config_watch_filter("nested/.omo", &filters)); }
#[test]
fn anchored_nested_path_does_not_match_suffix() { let filters = vec!["/.omo/omo.jsonc".into()]; assert!(matches_config_watch_filter(".omo/omo.jsonc", &filters)); assert!(!matches_config_watch_filter("worktree/.omo/omo.jsonc", &filters)); }
#[test]
fn unanchored_filters_match_suffix_and_basename() { assert!(matches_config_watch_filter("nested/.omo", &[".omo".into()])); assert!(matches_config_watch_filter("theme.js", &["*.js".into()])); assert!(!matches_config_watch_filter("theme.rs", &["*.js".into()])); }
#[test]
fn later_registration_replaces_earlier_in_place() { let first = ConfigWatchRegistration { id:"r".into(), display_name:"first".into(), targets:vec![] }; let replacement = ConfigWatchRegistration { display_name:"last".into(), ..first.clone() }; assert_eq!(resolve_config_watch_registrations([first, replacement.clone()]), vec![replacement]); }
#[test]
fn duplicate_veto_reason_does_not_notify_twice() { let mut deferral = ReloadVetoDeferral::default(); assert!(deferral.defer(Some("worker")).is_some()); assert!(deferral.defer(Some("worker")).is_none()); assert!(deferral.defer(Some("other")).is_some()); }
#[test]
fn reset_allows_same_veto_reason_again() { let mut deferral = ReloadVetoDeferral::default(); deferral.defer(None); deferral.reset(); assert!(deferral.defer(None).is_some()); }
