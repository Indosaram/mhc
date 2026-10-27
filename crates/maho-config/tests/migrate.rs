mod common;

use common::Tree;
use maho_config::migrate::lock::LockRecord;
use maho_config::migrate::plans::{
    CATEGORY_DEEP_SPLIT, CODEX_CONFIG_JSONC, HARNESS_NATIVE_RENAME, REASONING_UNIFICATION,
    SUBSCRIPTION_PROVIDER_RENAME, run_startup_migration_with,
};
use maho_config::migrate::transforms::provider_rename;
use maho_config::migrate::{Boundary, MigrationEnv};
use maho_config::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const NOW: i64 = 1_800_000_000_000;

fn process() -> MigrationEnv {
    MigrationEnv {
        now_ms: Arc::new(|| NOW),
        pid: 4242,
        is_alive: Arc::new(|_| false),
        ..MigrationEnv::default()
    }
}

fn run(t: &Tree, cwd: &Path, env: MigrationEnv) -> StartupMigration {
    run_startup_migration_with(cwd, &t.env(), Some(&t.home), env)
}

fn read(path: &Path) -> Value {
    maho_support::parse_jsonc(&std::fs::read_to_string(path).expect("read")).expect("jsonc")
}

fn backups(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
        })
        .collect()
}

#[test]
fn reasoning_plan_folds_the_chain_and_backs_up() {
    let t = Tree::new();
    let original = "{\n  // mine\n  \"categories\": {\"quick\": {\"model\": \"a/b\", \"fallback_models\": [\"c/d\"]}}\n}\n";
    let path = t.write("home/.omo/omo.jsonc", original);
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None, "{result:?}");
    let value = read(&path);
    assert_eq!(
        value["categories"]["quick"],
        json!({"models": ["a/b", "c/d"]})
    );
    assert_eq!(value["_migrations"], json!([REASONING_UNIFICATION]));
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("// mine"));
    let found = backups(&t.home.join(".omo"), "omo.jsonc.bak.");
    assert_eq!(found.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&found[0]).expect("backup"),
        original
    );
    assert!(!t.home.join(".omo/.migration.lock").exists());
    assert!(!t.home.join(".omo/.migration-journal.json").exists());
}

#[test]
fn marked_file_is_skipped_and_byte_identical() {
    let t = Tree::new();
    let original = format!(
        "{{\"categories\": {{\"quick\": {{\"model\": \"a/b\", \"fallback_models\": [\"c/d\"]}}}}, \"_migrations\": [\"{REASONING_UNIFICATION}\"]}}"
    );
    let path = t.write("home/.omo/omo.jsonc", &original);
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None);
    assert!(result.plans.iter().all(|p| p.status == PlanStatus::Skipped));
    assert_eq!(std::fs::read_to_string(path).expect("read"), original);
}

#[test]
fn predicates_skip_plans_without_legacy_content() {
    let t = Tree::new();
    let path = t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"quick": {"model": "openai/gpt-x"}}}"#,
    );
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None);
    assert_eq!(read(&path)["_migrations"], json!([REASONING_UNIFICATION]));
    for id in [
        CATEGORY_DEEP_SPLIT,
        HARNESS_NATIVE_RENAME,
        SUBSCRIPTION_PROVIDER_RENAME,
    ] {
        assert!(
            result
                .plans
                .iter()
                .filter(|p| p.id == id)
                .all(|p| p.status == PlanStatus::Skipped)
        );
    }
}

#[test]
fn legacy_names_are_rewritten_on_disk() {
    let t = Tree::new();
    let path = t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"deep": {"model": "openai-codex/gpt-x"}}, "[senpi]": {"model_profile": "claude-sdk-oauth/m"},
            "agents": {"a": {"model": "openai/gpt-x"}}}"#,
    );
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None, "{result:?}");
    let value = read(&path);
    assert_eq!(
        value["categories"]["deep-low"]["model"],
        json!("chatgpt-subscription/gpt-x")
    );
    assert_eq!(
        value["[native]"]["model_profile"],
        json!("anthropic-subscription/m")
    );
    assert_eq!(value["agents"]["a"]["model"], json!("openai/gpt-x"));
    assert_eq!(
        value["_migrations"],
        json!([
            REASONING_UNIFICATION,
            CATEGORY_DEEP_SPLIT,
            HARNESS_NATIVE_RENAME,
            SUBSCRIPTION_PROVIDER_RENAME
        ])
    );
}

#[test]
fn provider_rename_rewrites_values_and_keys() {
    let (out, diagnostics) = provider_rename(&json!({
        "categories": {"quick": {"model": "openai-codex/gpt-x"}},
        "task": {"provider_concurrency": {"claude-sdk-oauth": 2, "openai": 1}},
        "x": "openai/gpt-x"
    }));
    assert_eq!(
        out["categories"]["quick"]["model"],
        json!("chatgpt-subscription/gpt-x")
    );
    assert_eq!(
        out["task"]["provider_concurrency"],
        json!({"anthropic-subscription": 2, "openai": 1})
    );
    assert_eq!(out["x"], json!("openai/gpt-x"));
    assert!(
        diagnostics.contains(
            &"$.categories.quick.model: openai-codex/gpt-x renamed to chatgpt-subscription/gpt-x"
                .to_string()
        )
    );
}

#[test]
fn live_lock_reports_locked_and_writes_nothing() {
    let t = Tree::new();
    let original = r#"{"categories": {"quick": {"model": "a/b", "fallback_models": ["c/d"]}}}"#;
    let path = t.write("home/.omo/omo.jsonc", original);
    let lock = t.write(
        "home/.omo/.migration.lock",
        &LockRecord {
            lease_expires_at: NOW + 30_000,
            pid: 7,
        }
        .to_line(),
    );
    let mut env = process();
    env.is_alive = Arc::new(|_| true);
    let result = run(&t, &t.home, env);
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("already running")),
        "{result:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), original);
    assert!(lock.exists());
}

#[test]
fn stale_lock_of_a_dead_process_is_reclaimed() {
    let t = Tree::new();
    let path = t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"quick": {"model": "a/b"}}}"#,
    );
    t.write(
        "home/.omo/.migration.lock",
        &LockRecord {
            lease_expires_at: NOW - 1,
            pid: 7,
        }
        .to_line(),
    );
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None);
    assert_eq!(read(&path)["_migrations"], json!([REASONING_UNIFICATION]));
    assert!(!t.home.join(".omo/.migration.lock").exists());
}

#[test]
fn garbage_lock_is_reclaimable_and_lock_record_round_trips() {
    let record = LockRecord {
        lease_expires_at: 5,
        pid: 9,
    };
    assert_eq!(LockRecord::parse(&record.to_line()), Some(record));
    assert!(record.to_line().ends_with('\n'));
    assert!(record.to_line().contains("\"leaseExpiresAt\""));
    let t = Tree::new();
    t.write("home/.omo/omo.jsonc", "{}");
    t.write("home/.omo/.migration.lock", "not json");
    assert_eq!(run(&t, &t.home, process()).error, None);
}

#[test]
fn interrupted_migration_is_resumed() {
    let t = Tree::new();
    t.write(
        "home/.omo/config.jsonc",
        r#"{"[senpi]": {"model_profile": "geeky-heavy"}}"#,
    );
    let mut env = process();
    env.hook = Some(Arc::new(|boundary| {
        if boundary == Boundary::TargetWritten {
            Err("simulated crash".into())
        } else {
            Ok(())
        }
    }));
    let first = run(&t, &t.home, env);
    assert!(
        first
            .error
            .as_deref()
            .is_some_and(|e| e.contains("simulated crash"))
    );
    assert!(t.home.join(".omo/.migration-journal.json").exists());
    assert!(t.home.join(".omo/config.jsonc").exists());

    let second = run(&t, &t.home, process());
    assert_eq!(second.error, None, "{second:?}");
    assert!(second.journal_resumed);
    assert!(!t.home.join(".omo/.migration-journal.json").exists());
    assert!(!t.home.join(".omo/config.jsonc").exists());
    let value = read(&t.home.join(".omo/omo.jsonc"));
    assert_eq!(value["[native]"]["model_profile"], json!("geeky-heavy"));
}

#[test]
fn strict_validation_failure_aborts_and_leaves_the_file() {
    let t = Tree::new();
    let original = r#"{"bogus": true, "categories": {"quick": {"model": "a/b"}}}"#;
    let path = t.write("home/.omo/omo.jsonc", original);
    let result = run(&t, &t.home, process());
    assert!(
        result.error.as_deref().is_some_and(|e| e.contains("bogus")),
        "{result:?}"
    );
    assert_eq!(std::fs::read_to_string(path).expect("read"), original);
    assert!(!t.home.join(".omo/.migration.lock").exists());
}

#[test]
fn retired_codegraph_is_removed() {
    let t = Tree::new();
    let path = t.write(
        "home/.omo/omo.jsonc",
        r#"{"[native]": {"codegraph": {"enabled": true}, "model_profile": "x"}}"#,
    );
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None, "{result:?}");
    let value = read(&path);
    assert!(value["[native]"].get("codegraph").is_none());
    let diagnostics: Vec<&String> = result.plans.iter().flat_map(|p| &p.diagnostics).collect();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("[native].codegraph") && d.contains("retired")),
        "{diagnostics:?}"
    );
}

#[test]
fn codex_config_is_imported_into_native() {
    let t = Tree::new();
    let source = t.write(
        "home/.omo/config.jsonc",
        r#"{"[senpi]": {"categories": {"quick": {"model": "p/q"}}}}"#,
    );
    let result = run(&t, &t.home, process());
    assert_eq!(result.error, None, "{result:?}");
    let value = read(&t.home.join(".omo/omo.jsonc"));
    assert_eq!(
        value["[native]"]["categories"]["quick"]["model"],
        json!("p/q")
    );
    let markers = value["_migrations"].as_array().expect("markers");
    assert!(markers.contains(&json!(CODEX_CONFIG_JSONC)));
    assert!(!source.exists());
    assert_eq!(result.migrated_from, vec![source.clone()]);
    let backup_dirs = backups(&t.home.join(".omo"), "migration-backup-");
    assert_eq!(backup_dirs.len(), 1);
    assert!(
        backup_dirs[0]
            .to_string_lossy()
            .ends_with("-opencode-config")
    );
    assert!(backup_dirs[0].join(".omo/config.jsonc").exists());
}

#[test]
fn project_file_failure_keeps_earlier_effects() {
    let t = Tree::new();
    let user = t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"quick": {"model": "a/b"}}}"#,
    );
    let project = t.write("home/p/.omo/omo.jsonc", r#"{"typo_key": 1}"#);
    let cwd = t.mkdir("home/p");
    let result = run(&t, &cwd, process());
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("typo_key"))
    );
    assert_eq!(read(&user)["_migrations"], json!([REASONING_UNIFICATION]));
    assert_eq!(read(&project), json!({"typo_key": 1}));
}

#[test]
fn missing_home_is_an_error() {
    let t = Tree::new();
    let result = run_startup_migration_with(&t.home, &t.env(), None, process());
    assert!(result.error.is_some());
}
