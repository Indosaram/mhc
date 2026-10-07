use std::collections::BTreeMap;

use maho_omo_config_resolution::{SenpiConfigDiagnostic, SenpiOmoConfigResult};
use maho_omo_config_startup::{
    DevinSwe2Policy, SenpiStartupMigrationResult, StartupNoticeKind, notification_messages,
    run_senpi_startup_migration,
};
use omo_config_core::{LoadOmoConfigResult, MigrationStatus, OmoConfigDiagnostic};
use serde_json::{Value, json};

fn empty_config() -> SenpiOmoConfigResult {
    SenpiOmoConfigResult {
        loaded: LoadOmoConfigResult {
            config: serde_json::Map::new(),
            diagnostics: Vec::new(),
            layers: Vec::new(),
            profile: None,
            sources: Vec::new(),
        },
        config: json!({}),
        diagnostics: Vec::new(),
    }
}

fn config_with_diagnostics(config: Value, messages: &[&str]) -> SenpiOmoConfigResult {
    let mut result = empty_config();
    result.config = config;
    result.diagnostics = messages
        .iter()
        .map(|message| {
            SenpiConfigDiagnostic::Config(OmoConfigDiagnostic {
                kind: "parse",
                path: "/home/alice/.maho/omo.jsonc".into(),
                message: (*message).into(),
                issue_paths: Vec::new(),
            })
        })
        .collect();
    result
}

fn quiet_migration() -> SenpiStartupMigrationResult {
    SenpiStartupMigrationResult::default()
}

fn is_unserved_devin(selector: &str) -> bool {
    matches!(
        selector,
        "devin/swe-2-low" | "devin/swe-2-high-lite" | "devin/swe-2"
    )
}

fn devin_policy<'a>() -> DevinSwe2Policy<'a> {
    DevinSwe2Policy {
        served_lanes: Some(&["swe-2-medium", "swe-2-high", "swe-2-max"]),
        is_unserved: Some(&is_unserved_devin),
    }
}

#[test]
fn notification_messages_report_migration_and_config_diagnostics_with_the_native_prefix() {
    let migration = SenpiStartupMigrationResult {
        migrated_from: vec!["/home/alice/.config/opencode/oh-my-openagent.jsonc".into()],
        ..quiet_migration()
    };
    let config = config_with_diagnostics(json!({}), &["JSONC parse error"]);

    let notices = notification_messages(&migration, &config, &DevinSwe2Policy::unavailable());

    assert_eq!(notices.len(), 2);
    assert_eq!(
        notices[0].message,
        "OmO Native: migrated legacy configuration from /home/alice/.config/opencode/oh-my-openagent.jsonc"
    );
    assert_eq!(notices[0].kind, StartupNoticeKind::Info);
    assert_eq!(
        notices[1].message,
        "OmO Native: configuration diagnostics: JSONC parse error"
    );
    assert_eq!(notices[1].kind, StartupNoticeKind::Warning);
}

#[test]
fn notification_messages_report_an_error_before_every_other_clause() {
    let migration = SenpiStartupMigrationResult {
        error: Some("failure".into()),
        journal_resumed: true,
        ..quiet_migration()
    };

    let notices = notification_messages(
        &migration,
        &empty_config(),
        &DevinSwe2Policy::unavailable(),
    );

    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].message, "OmO Native: configuration migration: failure");
    assert_eq!(notices[0].kind, StartupNoticeKind::Warning);
}

#[test]
fn notification_messages_report_a_resumed_journal_when_nothing_migrated() {
    let migration = SenpiStartupMigrationResult {
        journal_resumed: true,
        ..quiet_migration()
    };

    let notices = notification_messages(
        &migration,
        &empty_config(),
        &DevinSwe2Policy::unavailable(),
    );

    assert_eq!(
        notices,
        vec![maho_omo_config_startup::StartupNotice {
            message: "OmO Native: recovered an interrupted configuration migration".into(),
            kind: StartupNoticeKind::Info,
        }]
    );
}

#[test]
fn notification_messages_join_every_migration_diagnostic_once() {
    let migration = SenpiStartupMigrationResult {
        results: vec![
            omo_config_core::MigrationRunResult {
                diagnostics: vec!["first".into()],
                journal_resumed: false,
                preview: None,
                status: MigrationStatus::Migrated,
            },
            omo_config_core::MigrationRunResult {
                diagnostics: vec!["second".into()],
                journal_resumed: false,
                preview: None,
                status: MigrationStatus::Migrated,
            },
        ],
        ..quiet_migration()
    };

    let notices = notification_messages(
        &migration,
        &empty_config(),
        &DevinSwe2Policy::unavailable(),
    );

    assert_eq!(
        notices[0].message,
        "OmO Native: configuration migration: first; second"
    );
}

#[test]
fn notification_messages_report_unserved_devin_selectors_with_their_config_paths() {
    let config = config_with_diagnostics(
        json!({
            "categories": {
                "quick": { "model": "devin/swe-2-low" },
                "deep-low": { "models": ["devin/swe-2-max", { "model": "devin/swe-2-high-lite" }] },
                "writing": { "model": "devin/swe-2-high", "fallback_models": ["devin/swe-2"] },
            },
            "agents": { "scout": { "model": "devin/swe-2-medium" } },
        }),
        &[],
    );

    let notices = notification_messages(&quiet_migration(), &config, &devin_policy());

    assert_eq!(
        notices,
        vec![maho_omo_config_startup::StartupNotice {
            message: "OmO Native: Devin does not serve devin/swe-2-low (categories.quick.model), devin/swe-2-high-lite (categories.deep-low.models[1]), devin/swe-2 (categories.writing.fallback_models[0]); SWE-2 runs as devin/swe-2-medium, devin/swe-2-high or devin/swe-2-max".into(),
            kind: StartupNoticeKind::Warning,
        }]
    );
}

#[test]
fn notification_messages_stay_silent_without_a_devin_policy() {
    let config = config_with_diagnostics(
        json!({ "categories": { "quick": { "model": "devin/swe-2-low" } } }),
        &[],
    );

    let notices = notification_messages(&quiet_migration(), &config, &DevinSwe2Policy::unavailable());

    assert!(notices.is_empty());
}

#[test]
fn missing_home_reports_the_pinned_error_and_touches_nothing() {
    let result = run_senpi_startup_migration("/tmp/project", &BTreeMap::new(), "");

    assert_eq!(
        result.error.as_deref(),
        Some("Cannot migrate configuration because no home directory is available")
    );
    assert!(result.results.is_empty());
    assert!(result.migrated_from.is_empty());
    assert!(!result.journal_resumed);
}

#[test]
fn notification_messages_use_the_real_devin_producer_for_selector_boundaries() {
    // Machine contract: served lanes are not a boundary violation; an unserved selector is reported
    // with its exact config path. Asserted against the real model-core producer, not stub tables.
    let served = config_with_diagnostics(
        json!({ "categories": { "quick": { "model": "devin/swe-2-medium" } } }),
        &[],
    );
    assert!(notification_messages(&quiet_migration(), &served, &DevinSwe2Policy::native()).is_empty());

    let unserved = config_with_diagnostics(
        json!({
            "categories": { "quick": { "model": "devin/swe-2-low" } },
            "agents": { "scout": { "models": [{ "model": "devin/swe-2-high-lite" }] } },
        }),
        &[],
    );
    let notices = notification_messages(&quiet_migration(), &unserved, &DevinSwe2Policy::native());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, StartupNoticeKind::Warning);
    let message = &notices[0].message;
    assert!(message.contains("devin/swe-2-low"));
    assert!(message.contains("categories.quick.model"));
    assert!(message.contains("devin/swe-2-high-lite"));
    assert!(message.contains("agents.scout.models[0]"));
}
