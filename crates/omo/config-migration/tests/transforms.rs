use std::collections::BTreeMap;

use config_migration::{
    CONFIG_JSONC_MIGRATION_ID, ConfigMigrationDiscoveryOptions, DiscoveredLegacyConfigSource,
    LegacyConfigSourceKind, LoadedLegacyConfigSource, OPENCODE_CONFIG_MIGRATION_ID,
    OpenCodeTransformScope, PathOperations, Platform, TransformConfigJsoncSourcesInput,
    TransformOpenCodeSourcesInput, discover_legacy_config_groups, transform_config_jsonc_sources,
    transform_open_code_sources,
};
use omo_config_core::internal::validate::safe_parse;
use omo_config_core::schema::config::omo_config_schema;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/code-yeongyu/oh-my-openagent/dev/assets/omo.schema.json";

fn loaded(source: &DiscoveredLegacyConfigSource, value: Value) -> LoadedLegacyConfigSource {
    LoadedLegacyConfigSource {
        path: source.path.clone(),
        value,
    }
}

fn source(kind: LegacyConfigSourceKind, path: &str) -> DiscoveredLegacyConfigSource {
    DiscoveredLegacyConfigSource {
        base_root: None,
        config_path: path.to_string(),
        is_active_profile: false,
        kind,
        path: path.to_string(),
        precedence: 0,
        profile: None,
        project_root: None,
    }
}

fn user_root() -> DiscoveredLegacyConfigSource {
    DiscoveredLegacyConfigSource {
        base_root: Some("/home/alice/.config/opencode".into()),
        ..source(
            LegacyConfigSourceKind::UserConfig,
            "/home/alice/.config/opencode/oh-my-openagent.jsonc",
        )
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected object, got {other}"),
    }
}

#[test]
fn config_jsonc_with_both_omo_and_senpi_keeps_senpi_and_reports_the_overlap() {
    // given
    let config = source(
        LegacyConfigSourceKind::ConfigJsonc,
        "/home/alice/.maho/config.jsonc",
    );
    let sources = vec![loaded(
        &config,
        json!({
            "$schema": "https://legacy.example/config.schema.json",
            "codegraph": { "excluded_roots": ["/generated"] },
            "[opencode]": { "nested": { "value": true } },
            "[codex]": { "codegraph": { "daemon": false } },
            "[omo]": { "agents": { "oracle": { "model": "legacy" } } },
            "[senpi]": { "agents": { "oracle": { "model": "current" } } },
            "_migrations": ["legacy-file-marker"],
            "appliedMigrations": ["legacy-top-level-marker"],
        }),
    )];

    // when
    let result = transform_config_jsonc_sources(&TransformConfigJsoncSourcesInput {
        discovered: std::slice::from_ref(&config),
        sources: &sources,
    });

    // then
    assert_eq!(
        Value::Object(result.document),
        json!({
            "$schema": SCHEMA_URL,
            "codegraph": { "excluded_roots": ["/generated"] },
            "[opencode]": { "nested": { "value": true } },
            "[codex]": { "codegraph": { "daemon": false } },
            "[senpi]": { "agents": { "oracle": { "model": "current" } } },
            "legacy_migrations": {
                "/home/alice/.maho/config.jsonc": ["legacy-file-marker", "legacy-top-level-marker"],
            },
        })
    );
    assert_eq!(
        result.diagnostics,
        vec!["conflict: [senpi] legacy [omo] kept [senpi]".to_string()]
    );
}

#[test]
fn root_profile_config_jsonc_and_project_sources_produce_golden_documents_that_parse_through_the_schema()
 {
    // given
    let fixture = tempfile::tempdir().expect("tempdir");
    let home_dir = fixture.path().join("home").to_string_lossy().into_owned();
    let project_dir = fixture
        .path()
        .join("project")
        .to_string_lossy()
        .into_owned();
    let files = [
        format!("{home_dir}/.config/opencode/oh-my-openagent.jsonc"),
        format!("{home_dir}/.config/opencode/profiles/focused/oh-my-openagent.jsonc"),
        format!("{home_dir}/.config/opencode/profiles/kimi/oh-my-opencode.json"),
        format!("{home_dir}/.maho/config.jsonc"),
        format!("{project_dir}/.opencode/oh-my-openagent.jsonc"),
    ];
    for path in &files {
        let path = std::path::Path::new(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, "{}").expect("write");
    }
    let xdg = format!("{home_dir}/.config");
    let environment: BTreeMap<String, String> =
        [("HOME", home_dir.as_str()), ("XDG_CONFIG_HOME", &xdg)]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
    let groups = discover_legacy_config_groups(&ConfigMigrationDiscoveryOptions {
        cwd: &project_dir,
        environment: &environment,
        file_system: None,
        home_dir: &home_dir,
        path_operations: PathOperations::Posix,
        platform: Some(Platform::Linux),
        tauri_config_dirs: None,
    })
    .expect("discovery");
    let opencode_group = groups
        .iter()
        .find(|group| group.id == OPENCODE_CONFIG_MIGRATION_ID)
        .expect("opencode group");
    let config_jsonc_group = groups
        .iter()
        .find(|group| group.id == CONFIG_JSONC_MIGRATION_ID)
        .expect("config.jsonc group");
    let find = |predicate: &dyn Fn(&DiscoveredLegacyConfigSource) -> bool| {
        opencode_group
            .sources
            .iter()
            .find(|candidate| predicate(candidate))
            .expect("fixture source")
    };
    let root = find(&|s| s.kind == LegacyConfigSourceKind::UserConfig);
    let focused = find(&|s| s.profile.as_deref() == Some("focused"));
    let kimi = find(&|s| s.profile.as_deref() == Some("kimi"));
    let project = find(&|s| s.kind == LegacyConfigSourceKind::ProjectConfig);
    let project_root = project.project_root.clone().expect("project root");
    let config_jsonc = config_jsonc_group
        .sources
        .iter()
        .find(|s| s.kind == LegacyConfigSourceKind::ConfigJsonc)
        .expect("config.jsonc source");
    let opencode = transform_open_code_sources(&TransformOpenCodeSourcesInput {
        discovered: &opencode_group.sources,
        scope: &OpenCodeTransformScope::User,
        sources: &[
            loaded(
                root,
                json!({
                    "$schema": "https://legacy.example/schema.json",
                    "agents": { "oracle": { "model": "old-model" } },
                    "categories": { "deep": { "model": "old-model" } },
                }),
            ),
            loaded(
                focused,
                json!({
                    "agents": { "oracle": { "model": "old-model" } },
                    "categories": { "deep": { "model": "focused-model" } },
                }),
            ),
            loaded(
                kimi,
                json!({ "agents": { "oracle": { "model": "kimi-model" } } }),
            ),
            loaded(
                project,
                json!({ "agents": { "oracle": { "model": "project-model" } } }),
            ),
        ],
    });
    let config_jsonc_document = transform_config_jsonc_sources(&TransformConfigJsoncSourcesInput {
        discovered: &config_jsonc_group.sources,
        sources: &[loaded(
            config_jsonc,
            json!({
                "$schema": "https://legacy.example/config.schema.json",
                "codegraph": { "excluded_roots": ["/generated", "/vendor"] },
                "[opencode]": { "background_task": { "enabled": true } },
                "[codex]": { "codegraph": { "daemon": false } },
                "[omo]": { "agents": { "oracle": { "model": "senpi-model" } } },
            }),
        )],
    });
    let project_document = transform_open_code_sources(&TransformOpenCodeSourcesInput {
        discovered: &opencode_group.sources,
        scope: &OpenCodeTransformScope::Project { project_root },
        sources: &[loaded(
            project,
            json!({ "agents": { "oracle": { "model": "project-model" } } }),
        )],
    });

    // when
    let mut user_document = config_jsonc_document.document.clone();
    user_document.extend(opencode.document.clone());
    let mut merged_opencode = config_jsonc_document.document["[opencode]"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    merged_opencode.extend(object(opencode.document["[opencode]"].clone()));
    user_document.insert("[opencode]".into(), Value::Object(merged_opencode));
    let user_result = safe_parse(&omo_config_schema(), &Value::Object(user_document.clone()));
    let project_result = safe_parse(
        &omo_config_schema(),
        &Value::Object(project_document.document.clone()),
    );

    // then
    assert!(user_result.is_ok(), "user document parses: {user_result:?}");
    assert!(
        project_result.is_ok(),
        "project document parses: {project_result:?}"
    );
    let mut actual = user_document;
    actual.sort_keys();
    let mut expected = object(json!({
        "$schema": SCHEMA_URL,
        "codegraph": { "excluded_roots": ["/generated", "/vendor"] },
        "[opencode]": {
            "agents": { "oracle": { "model": "old-model" } },
            "background_task": { "enabled": true },
            "categories": { "deep": { "model": "old-model" } },
        },
        "[codex]": { "codegraph": { "daemon": false } },
        "[senpi]": { "agents": { "oracle": { "model": "senpi-model" } } },
        "profiles": {
            "focused": { "[opencode]": { "categories": { "deep": { "model": "focused-model" } } } },
            "kimi": { "[opencode]": { "agents": { "oracle": { "model": "kimi-model" } } } },
        },
    }));
    expected.sort_keys();
    assert_eq!(Value::Object(actual), Value::Object(expected));
    assert_eq!(
        project_document.document["[opencode]"],
        json!({ "agents": { "oracle": { "model": "project-model" } } })
    );
}

#[test]
fn identical_profile_subtree_and_reverted_migration_sidecar_keep_only_the_delta_and_user_left_value()
 {
    // given
    let root = user_root();
    let profile = DiscoveredLegacyConfigSource {
        base_root: root.base_root.clone(),
        profile: Some("kimi".into()),
        ..source(
            LegacyConfigSourceKind::ProfileConfig,
            "/home/alice/.config/opencode/profiles/kimi/oh-my-openagent.jsonc",
        )
    };
    let sidecar = DiscoveredLegacyConfigSource {
        kind: LegacyConfigSourceKind::MigrationSidecar,
        path: format!("{}.migrations.json", root.path),
        ..root.clone()
    };

    // when
    let result = transform_open_code_sources(&TransformOpenCodeSourcesInput {
        discovered: &[root.clone(), profile.clone(), sidecar.clone()],
        scope: &OpenCodeTransformScope::User,
        sources: &[
            loaded(
                &root,
                json!({ "agents": { "oracle": { "model": "reverted-old-model" }, "atlas": { "model": "same" } } }),
            ),
            loaded(
                &profile,
                json!({ "agents": { "oracle": { "model": "reverted-old-model" }, "atlas": { "model": "different" } } }),
            ),
            loaded(
                &sidecar,
                json!({ "appliedMigrations": ["model-version:reverted-old-model->new-model"] }),
            ),
        ],
    });

    // then
    assert_eq!(
        result.document["[opencode]"],
        json!({ "agents": { "atlas": { "model": "same" }, "oracle": { "model": "reverted-old-model" } } })
    );
    assert_eq!(
        result.document["profiles"],
        json!({ "kimi": { "[opencode]": { "agents": { "atlas": { "model": "different" } } } } })
    );
    assert_eq!(
        result.document["legacy_migrations"],
        json!({ root.path.clone(): ["model-version:reverted-old-model->new-model"] })
    );
}

#[test]
fn legacy_config_keys_are_rewritten_to_their_current_omo_equivalents() {
    // given
    let root = user_root();

    // when
    let result = transform_open_code_sources(&TransformOpenCodeSourcesInput {
        discovered: std::slice::from_ref(&root),
        scope: &OpenCodeTransformScope::User,
        sources: &[loaded(
            &root,
            json!({
                "agents": {
                    "OmO": { "model": "anthropic/claude-opus-4-4" },
                    "oracle": { "model": "anthropic/claude-opus-4-4" },
                },
                "categories": { "deep": { "model": "anthropic/claude-opus-4-4" } },
                "disabled_agents": ["OmO"],
                "disabled_hooks": ["anthropic-auto-compact", "empty-message-sanitizer"],
                "experimental": { "hashline_edit": { "enabled": true } },
                "lsp": { "typescript": { "command": ["typescript-language-server", "--stdio"] } },
                "omo_agent": { "model": "provider/sisyphus" },
            }),
        )],
    });

    // then
    assert_eq!(
        result.document["[opencode]"],
        json!({
            "agents": {
                "oracle": { "model": "anthropic/claude-opus-4-8" },
                "sisyphus": { "model": "anthropic/claude-opus-4-8" },
            },
            "categories": { "deep": { "model": "anthropic/claude-opus-4-8" } },
            "disabled_agents": ["sisyphus"],
            "disabled_hooks": ["anthropic-context-window-limit-recovery"],
            "hashline_edit": { "enabled": true },
            "sisyphus_agent": { "model": "provider/sisyphus" },
        })
    );
    assert!(!result.document.contains_key("legacy_migrations"));
}
