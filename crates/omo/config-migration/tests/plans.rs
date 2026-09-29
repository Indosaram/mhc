use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use config_migration::{
    CONFIG_JSONC_MIGRATION_ID, ConfigMigrationDiscoveryOptions,
    CreateLegacyConfigMigrationPlansOptions, ExecuteLegacyConfigMigrationPlanOptions,
    LegacyConfigMigrationPlan, OPENCODE_CONFIG_MIGRATION_ID, PathOperations, Platform,
    REASONING_UNIFICATION_MIGRATION_ID, create_legacy_config_migration_plans,
    execute_legacy_config_migration_plan, transform_reasoning_unification,
};
use omo_config_core::{MigrationBoundary, MigrationSourceDescriptor, MigrationStatus};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

const FIXTURE_INPUT: &str =
    include_str!("fixtures/2026-08-reasoning-unification/fixture-input.json");
const FIXTURE_EXPECTED: &str =
    include_str!("fixtures/2026-08-reasoning-unification/fixture-expected.json");
const REASONING_CONFLICT: &str =
    r#"conflict: categories.conflict dropped variant="high" kept reasoningEffort="xhigh""#;

fn fixture(name: &str) -> Value {
    serde_json::from_str(name).expect("fixture json")
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

fn home_env(home_dir: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("HOME".to_string(), home_dir.to_string())])
}

fn parse_jsonc(path: &Path) -> Value {
    let content = fs::read_to_string(path).expect("read target");
    omo_config_core::parse_jsonc_safe(&content)
        .data
        .expect("jsonc data")
}

fn execute(
    plan: &LegacyConfigMigrationPlan,
    home_dir: &str,
    dry_run: bool,
) -> omo_config_core::MigrationRunResult {
    execute_legacy_config_migration_plan(
        plan,
        ExecuteLegacyConfigMigrationPlanOptions {
            dry_run,
            env: Some(home_env(home_dir)),
            ..Default::default()
        },
    )
    .expect("migration runs")
}

fn plans_for(
    home_dir: &str,
    cwd: &str,
    xdg: Option<&str>,
    timestamp: &str,
) -> Vec<LegacyConfigMigrationPlan> {
    let mut environment = home_env(home_dir);
    if let Some(xdg) = xdg {
        environment.insert("XDG_CONFIG_HOME".into(), xdg.into());
    }
    create_legacy_config_migration_plans(&CreateLegacyConfigMigrationPlansOptions {
        backup_timestamp: Some(timestamp),
        discovery: ConfigMigrationDiscoveryOptions {
            cwd,
            environment: &environment,
            file_system: None,
            home_dir,
            path_operations: PathOperations::Posix,
            platform: Some(Platform::Linux),
            tauri_config_dirs: None,
        },
    })
    .expect("planning succeeds")
}

fn tree(root: &Path) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            entries.push(path);
        }
    }
    entries.sort();
    entries
}

fn backup_paths(plan: &LegacyConfigMigrationPlan) -> Vec<String> {
    plan.sources
        .iter()
        .map(|source| source.backup_path.clone().unwrap_or_default())
        .collect()
}

#[test]
fn executor_with_a_false_predicate_creates_no_directories_or_files() {
    // given
    let fixture_root = tempfile::tempdir().expect("tempdir");
    let root = fixture_root.path();
    let home_dir = root.join("home");
    let source_path = root.join("legacy").join("config.jsonc");
    let target_path = home_dir.join(".maho").join("omo.jsonc");
    let backup_path = root.join("backup").join("config.jsonc");
    write(&source_path, "{}");
    write(&target_path, r#"{"_migrations":["already-migrated"]}"#);
    let entries_before = tree(root);
    let transform: config_migration::LegacyConfigMigrationTransform = std::rc::Rc::new(|_| {
        Ok(config_migration::ConfigMigrationTransformResult {
            diagnostics: Vec::new(),
            document: Map::new(),
        })
    });
    let plan = LegacyConfigMigrationPlan {
        id: "already-migrated".into(),
        inspect: transform.clone(),
        mode: omo_config_core::MigrationMode::Merge,
        sources: vec![MigrationSourceDescriptor::with_backup(
            text(&source_path),
            text(&backup_path),
        )],
        target_path: text(&target_path),
        transform,
    };

    // when
    let result = execute(&plan, &text(&home_dir), false);

    // then
    assert_eq!(result.status, MigrationStatus::Skipped);
    assert!(!root.join("backup").exists());
    assert_eq!(tree(root), entries_before);
}

#[test]
fn user_profile_config_jsonc_project_and_sidecar_fixtures_keep_distinct_markers_and_exact_backups()
{
    // given
    let fixture_root = tempfile::tempdir().expect("tempdir");
    let home = fixture_root.path().join("home");
    let project = fixture_root.path().join("project");
    let root_path = home.join(".config/opencode/oh-my-openagent.jsonc");
    let sidecar_path = PathBuf::from(format!("{}.migrations.json", text(&root_path)));
    let profile_path = home.join(".config/opencode/profiles/kimi/oh-my-openagent.jsonc");
    let config_jsonc_path = home.join(".maho/config.jsonc");
    let config_sidecar_path =
        PathBuf::from(format!("{}.migrations.json", text(&config_jsonc_path)));
    let project_path = project.join(".opencode/oh-my-opencode.json");
    let unrelated_path = PathBuf::from(format!("{}.bak.unrelated", text(&root_path)));
    let timestamp = "2026-07-27T12-34-56-789Z";
    write(
        &root_path,
        r#"{"agents":{"oracle":{"model":"reverted-old-model"}}}"#,
    );
    write(
        &sidecar_path,
        r#"{"appliedMigrations":["model-version:reverted-old-model->new-model"]}"#,
    );
    write(
        &profile_path,
        r#"{"agents":{"oracle":{"model":"kimi-model"}}}"#,
    );
    write(
        &config_jsonc_path,
        r#"{"codegraph":{"excluded_roots":["/generated"]}}"#,
    );
    write(
        &config_sidecar_path,
        r#"{"appliedMigrations":["legacy-config-jsonc"]}"#,
    );
    write(
        &project_path,
        r#"{"agents":{"oracle":{"model":"project-model"}}}"#,
    );
    write(&unrelated_path, "keep");
    let canonical_home = text(&fs::canonicalize(&home).expect("home"));
    let canonical_project = text(&fs::canonicalize(&project).expect("project"));
    let canonical_root = text(&fs::canonicalize(&root_path).expect("root"));
    let canonical_config_jsonc = text(&fs::canonicalize(&config_jsonc_path).expect("config"));
    let home_dir = text(&home);
    let xdg = text(&home.join(".config"));

    // when
    let plans = plans_for(&home_dir, &text(&project), Some(&xdg), timestamp);
    let find = |id: &str, predicate: &dyn Fn(&MigrationSourceDescriptor) -> bool| {
        plans
            .iter()
            .find(|plan| plan.id == id && plan.sources.iter().any(predicate))
            .expect("expected plan")
    };
    let opencode_user_plan = find(OPENCODE_CONFIG_MIGRATION_ID, &|s| s.path == canonical_root);
    let config_jsonc_plan = find(CONFIG_JSONC_MIGRATION_ID, &|s| {
        s.path == canonical_config_jsonc
    });
    let project_plan = find(OPENCODE_CONFIG_MIGRATION_ID, &|s| {
        s.path.ends_with("oh-my-opencode.json")
    });
    let config_result = execute(config_jsonc_plan, &home_dir, false);
    let opencode_result = execute(opencode_user_plan, &home_dir, false);
    let project_result = execute(project_plan, &home_dir, false);

    // then
    assert_eq!(config_result.status, MigrationStatus::Migrated);
    assert_eq!(opencode_result.status, MigrationStatus::Migrated);
    assert_eq!(project_result.status, MigrationStatus::Migrated);
    let user_document = parse_jsonc(&home.join(".maho/omo.jsonc"));
    assert_eq!(
        user_document["_migrations"],
        json!([CONFIG_JSONC_MIGRATION_ID, OPENCODE_CONFIG_MIGRATION_ID])
    );
    assert_eq!(
        user_document["[opencode]"],
        json!({ "agents": { "oracle": { "model": "reverted-old-model" } } })
    );
    assert_eq!(
        user_document["legacy_migrations"][canonical_root.as_str()],
        json!(["model-version:reverted-old-model->new-model"])
    );
    assert_eq!(
        user_document["legacy_migrations"][canonical_config_jsonc.as_str()],
        json!(["legacy-config-jsonc"])
    );
    let user_backup =
        format!("{canonical_home}/.maho/migration-backup-{timestamp}-opencode-config");
    assert_eq!(
        backup_paths(opencode_user_plan),
        vec![
            format!("{user_backup}/.config/opencode/oh-my-openagent.jsonc"),
            format!("{user_backup}/.config/opencode/oh-my-openagent.jsonc.migrations.json"),
            format!("{user_backup}/.config/opencode/profiles/kimi/oh-my-openagent.jsonc"),
        ]
    );
    assert_eq!(
        backup_paths(config_jsonc_plan),
        vec![
            format!("{user_backup}/.maho/config.jsonc"),
            format!("{user_backup}/.maho/config.jsonc.migrations.json"),
        ]
    );
    let project_backup =
        format!("{canonical_project}/.omo/migration-backup-{timestamp}/oh-my-opencode.json");
    assert_eq!(backup_paths(project_plan), vec![project_backup.clone()]);
    assert!(
        Path::new(&format!(
            "{user_backup}/.config/opencode/oh-my-openagent.jsonc"
        ))
        .exists()
    );
    assert!(Path::new(&project_backup).exists());
    assert!(unrelated_path.exists());
}

#[test]
fn overlapping_omo_and_senpi_blocks_report_the_conflict_in_preview_and_migration() {
    // given
    let fixture_root = tempfile::tempdir().expect("tempdir");
    let home = fixture_root.path().join("home");
    write(
        &home.join(".maho/config.jsonc"),
        r#"{"[omo]":{"agents":{"oracle":{"model":"legacy"}}},"[senpi]":{"agents":{"oracle":{"model":"current"}}}}"#,
    );
    let home_dir = text(&home);
    let xdg = text(&home.join(".config"));
    let plans = plans_for(&home_dir, &home_dir, Some(&xdg), "2026-07-27T12-34-56-789Z");
    let plan = plans
        .iter()
        .find(|candidate| candidate.id == CONFIG_JSONC_MIGRATION_ID)
        .expect("config.jsonc plan");

    // when
    let dry_run = execute(plan, &home_dir, true);
    let migrated = execute(plan, &home_dir, false);

    // then
    let conflict = "conflict: [senpi] legacy [omo] kept [senpi]".to_string();
    assert!(
        dry_run.diagnostics.contains(&conflict),
        "{:?}",
        dry_run.diagnostics
    );
    assert!(
        migrated.diagnostics.contains(&conflict),
        "{:?}",
        migrated.diagnostics
    );
}

#[test]
fn canonical_legacy_fixture_matches_expected_output_for_rules_one_through_eight() {
    // given
    let input = fixture(FIXTURE_INPUT);

    // when
    let result = transform_reasoning_unification(Some(&input)).expect("transform");

    // then
    assert_eq!(
        REASONING_UNIFICATION_MIGRATION_ID,
        "2026-08-reasoning-unification"
    );
    assert_eq!(Value::Object(result.document), fixture(FIXTURE_EXPECTED));
    assert!(result.diagnostics.contains(&REASONING_CONFLICT.to_string()));
}

#[test]
fn canonical_reasoning_outranking_legacy_keys_is_reported_as_the_winner() {
    // given
    let input = json!({ "categories": { "odd": {
        "model": "a/m", "reasoning": "low", "reasoningEffort": "xhigh", "variant": "high"
    } } });

    // when
    let result = transform_reasoning_unification(Some(&input)).expect("transform");

    // then
    assert_eq!(
        result.document["categories"]["odd"]["reasoning"],
        json!("low")
    );
    let journal = result
        .diagnostics
        .iter()
        .find(|entry| entry.contains("conflict"))
        .cloned()
        .unwrap_or_default();
    assert!(
        !journal.contains(r#"kept reasoningEffort="xhigh""#),
        "{journal}"
    );
}

#[test]
fn existing_user_omo_config_runs_dry_run_backup_journal_and_marker_through_the_engine() {
    // given
    let fixture_root = tempfile::tempdir().expect("tempdir");
    let home = fixture_root.path().join("home");
    let target_path = home.join(".maho/omo.jsonc");
    let input = fixture(FIXTURE_INPUT);
    write(
        &target_path,
        &serde_json::to_string_pretty(&input).expect("json"),
    );
    let home_dir = text(&home);
    let plans = plans_for(&home_dir, &home_dir, None, "2026-08-01T00-00-00-000Z");
    let plan = plans
        .iter()
        .find(|candidate| candidate.id == REASONING_UNIFICATION_MIGRATION_ID)
        .expect("reasoning plan");
    let journal_path = home.join(".maho/.migration-journal.json");
    let journal_diagnostics: RefCell<Option<Value>> = RefCell::new(None);

    // when
    let dry_run = execute(plan, &home_dir, true);
    let migrated = execute_legacy_config_migration_plan(
        plan,
        ExecuteLegacyConfigMigrationPlanOptions {
            env: Some(home_env(&home_dir)),
            on_boundary: Some(Box::new(|boundary| {
                if boundary == MigrationBoundary::JournalWritten {
                    let journal: Value =
                        serde_json::from_str(&fs::read_to_string(&journal_path).expect("journal"))
                            .expect("journal json");
                    *journal_diagnostics.borrow_mut() = journal.get("diagnostics").cloned();
                }
                Ok(())
            })),
            ..Default::default()
        },
    )
    .expect("migration runs");
    let document: Value =
        serde_json::from_str(&fs::read_to_string(&target_path).expect("target")).expect("json");

    // then
    assert_eq!(dry_run.status, MigrationStatus::Planned);
    assert_eq!(
        dry_run.preview.map(|preview| preview.transform),
        Some(fixture(FIXTURE_EXPECTED))
    );
    assert_eq!(migrated.status, MigrationStatus::Migrated);
    let mut expected = fixture(FIXTURE_EXPECTED);
    expected.as_object_mut().expect("object").insert(
        "_migrations".into(),
        json!([REASONING_UNIFICATION_MIGRATION_ID]),
    );
    assert_eq!(document, expected);
    let journal_diagnostics = journal_diagnostics
        .into_inner()
        .expect("journal diagnostics");
    assert!(
        journal_diagnostics
            .as_array()
            .is_some_and(|entries| entries.contains(&json!(REASONING_CONFLICT))),
        "{journal_diagnostics}"
    );
    let has_backup = fs::read_dir(home.join(".maho"))
        .expect("read .omo")
        .any(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with("omo.jsonc.bak.")
        });
    assert!(has_backup);
}

#[test]
fn typed_harness_blocks_and_profiles_recurse_while_opencode_rewrites_only_known_keys() {
    // given
    let input = json!({
        "[senpi]": { "categories": { "deep": { "model": "p/m high", "variant": "low" } } },
        "[codex]": { "agents": { "oracle": { "model": "p/m", "reasoningEffort": "none" } } },
        "[opencode]": {
            "agents": { "oracle": { "variant": "high", "native": { "variant": "untouched" } } },
            "categories": { "quick": { "textVerbosity": "low" } },
            "provider": { "variant": "untouched" },
        },
        "profiles": {
            "focused": { "categories": { "deep": { "model": "p/m(xhigh)", "maxTokens": 1 } } },
        },
    });

    // when
    let result = transform_reasoning_unification(Some(&input)).expect("transform");

    // then
    assert_eq!(
        Value::Object(result.document),
        json!({
            "[senpi]": { "categories": { "deep": { "model": "p/m:high", "reasoning": "low" } } },
            "[codex]": { "agents": { "oracle": { "model": "p/m", "reasoning": "off" } } },
            "[opencode]": {
                "agents": { "oracle": { "reasoning": "high", "native": { "variant": "untouched" } } },
                "categories": { "quick": { "textVerbosity": "low" } },
                "provider": { "variant": "untouched" },
            },
            "profiles": {
                "focused": { "categories": { "deep": { "model": "p/m:xhigh", "max_tokens": 1 } } },
            },
        })
    );
}
