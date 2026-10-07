//! Deterministic registered config migration/reload driver (four cases).
//!
//! Every case is bound to real current entry points: the `config-startup` component's migration path
//! `maho_omo_config_startup::run_senpi_startup_migration` and the loader the component consumes,
//! `omo_config_core::load_omo_config`. Scenarios are recorded in `authoring/config-startup.md` and
//! `authoring/config.md` BEFORE these edits; every command stays UNRUN until the global gate. Each case
//! uses a private `tempfile` HOME that is removed on drop; no daemon, PTY or subprocess runs.
use std::collections::BTreeMap;
use std::path::Path;

use maho_omo_config_startup::run_senpi_startup_migration;
use omo_config_core::{LoadOmoConfigOptions, MigrationStatus, load_omo_config};

fn home_env(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())])
}

fn migrated_document(home: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(home.join(".maho/omo.jsonc")).expect("migrated document");
    let parsed = omo_config_core::internal::jsonc::parse_jsonc_safe(&text);
    assert!(parsed.errors.is_empty(), "migrated document must parse: {:?}", parsed.errors);
    parsed.data.expect("migrated document is JSONC")
}

// Case 1 - fresh legacy migration writes the unified document.
#[test]
fn case1_fresh_legacy_migration_writes_the_unified_document() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(home.join(".config/opencode"))?;
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::write(home.join(".config/opencode/oh-my-openagent.jsonc"),
        r#"{"agents":{"finder":{"model":"provider/finder"}}}"#)?;
    std::fs::write(home.join(".maho/config.jsonc"), r#"{"codegraph":{"daemon":false}}"#)?;
    let env = home_env(&home);
    let result = run_senpi_startup_migration(&project.to_string_lossy(), &env, &home.to_string_lossy());
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(!result.migrated_from.is_empty());
    assert!(result.results.iter().any(|run| run.status == MigrationStatus::Migrated));
    let migrated = migrated_document(&home);
    assert!(migrated.get("codegraph").is_none(), "codegraph is retired: {migrated}");
    assert!(
        migrated.get("_migrations").and_then(serde_json::Value::as_array).is_some_and(|ids| !ids.is_empty()),
        "_migrations markers expected: {migrated}"
    );
    Ok(())
}

// Case 2 - idempotent second run.
#[test]
fn case2_second_run_reports_nothing_and_leaves_the_document_byte_identical() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::write(home.join(".maho/config.jsonc"), r#"{"codegraph":{"daemon":false}}"#)?;
    let env = home_env(&home);
    let cwd = project.to_string_lossy().into_owned();
    let home_path = home.to_string_lossy().into_owned();
    let first = run_senpi_startup_migration(&cwd, &env, &home_path);
    assert!(first.error.is_none(), "{:?}", first.error);
    let after_first = std::fs::read_to_string(home.join(".maho/omo.jsonc"))?;
    let second = run_senpi_startup_migration(&cwd, &env, &home_path);
    assert!(second.error.is_none(), "{:?}", second.error);
    assert!(second.migrated_from.is_empty());
    assert!(second.results.iter().all(|run| run.status != MigrationStatus::Migrated));
    let after_second = std::fs::read_to_string(home.join(".maho/omo.jsonc"))?;
    assert_eq!(after_first, after_second, "second run must not rewrite the document");
    Ok(())
}

// Case 3 - malformed legacy input: the failure is reported and no target is written.
#[test]
fn case3_malformed_legacy_input_reports_and_writes_no_target() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::write(home.join(".maho/config.jsonc"), "{\"task\":")?;
    let env = home_env(&home);
    let result = run_senpi_startup_migration(&project.to_string_lossy(), &env, &home.to_string_lossy());
    assert!(
        result.error.is_some() || result.results.iter().any(|run| !run.diagnostics.is_empty()),
        "a malformed legacy source must be reported"
    );
    assert!(!home.join(".maho/omo.jsonc").exists(), "no target is written for a malformed source");
    Ok(())
}

// Case 4 - reload: the loader reads the migrated document with codegraph retired.
#[test]
fn case4_migrated_document_reloads_without_codegraph() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::write(home.join(".maho/config.jsonc"), r#"{"codegraph":{"daemon":false}}"#)?;
    let env = home_env(&home);
    let cwd = project.to_string_lossy().into_owned();
    let first = run_senpi_startup_migration(&cwd, &env, &home.to_string_lossy());
    assert!(first.error.is_none(), "{:?}", first.error);
    let loaded = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env),
        harness: Some("senpi".into()),
        ..Default::default()
    });
    assert!(loaded.config.get("codegraph").is_none(), "reload must not resurrect codegraph");
    assert!(
        loaded.config.get("_migrations").and_then(serde_json::Value::as_array).is_some_and(|ids| !ids.is_empty()),
        "_migrations markers expected after reload"
    );
    Ok(())
}
