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
fn config_jsonc_with_both_omo_and_senpi_emits_native_and_reports_the_overlap() {
    // given
    let config = source(
        LegacyConfigSourceKind::ConfigJsonc,
        "/home/alice/.maho/config.jsonc",
    );
    let sources = vec![loaded(
        &config,
        json!({
            "$schema": "https://legacy.example/config.schema.json",
            "[opencode]": { "nested": { "value": true } },
            "[codex]": { "disabled_hooks": ["startup-toast"] },
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
            "[opencode]": { "nested": { "value": true } },
            "[codex]": { "disabled_hooks": ["startup-toast"] },
            "[native]": { "agents": { "oracle": { "model": "current" } } },
            "legacy_migrations": {
                "/home/alice/.maho/config.jsonc": ["legacy-file-marker", "legacy-top-level-marker"],
            },
        })
    );
    assert_eq!(
        result.diagnostics,
        vec!["conflict: [native] legacy [omo] kept [native]".to_string()]
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
                "[opencode]": { "background_task": { "enabled": true } },
                "[codex]": { "disabled_hooks": ["startup-toast"] },
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
        "[opencode]": {
            "agents": { "oracle": { "model": "old-model" } },
            "background_task": { "enabled": true },
            "categories": { "deep": { "model": "old-model" } },
        },
        "[codex]": { "disabled_hooks": ["startup-toast"] },
        "[native]": { "agents": { "oracle": { "model": "senpi-model" } } },
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

#[test]
fn category_deep_split_renames_every_reference_and_reports_it() {
    let document = json!({
        "categories": { "deep": { "model": "openai/gpt-6-astra", "reasoning": "high" } },
        "[senpi]": { "memory": { "reflection": { "category": "deep" } } },
        "teams": { "r": { "members": [{ "name": "one", "kind": "category", "category": "deep", "prompt": "go" }] } },
    });
    let result = config_migration::transform_category_deep_split(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({
            "categories": { "deep-low": { "model": "openai/gpt-6-astra", "reasoning": "high" } },
            "[senpi]": { "memory": { "reflection": { "category": "deep-low" } } },
            "teams": { "r": { "members": [{ "name": "one", "kind": "category", "category": "deep-low", "prompt": "go" }] } },
        })
    );
    assert_eq!(
        result.diagnostics,
        vec![
            "categories.deep renamed to deep-low".to_string(),
            "[senpi].memory.reflection.category renamed to deep-low".to_string(),
            "teams.r.members.0.category renamed to deep-low".to_string(),
        ]
    );
}

#[test]
fn category_deep_split_keeps_the_canonical_entry_and_reports_the_drop() {
    let document = json!({ "categories": { "deep": { "model": "legacy/model" }, "deep-low": { "model": "canonical/model" } } });
    let result = config_migration::transform_category_deep_split(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({ "categories": { "deep-low": { "model": "canonical/model" } } })
    );
    assert_eq!(
        result.diagnostics,
        vec!["categories.deep removed: deep-low is already configured".to_string()]
    );
}

#[test]
fn category_deep_split_leaves_a_clean_document_untouched() {
    let document = json!({ "categories": { "deep-high": { "model": "openai/gpt-6-astra" } }, "task": { "default_concurrency": 4 } });
    let result = config_migration::transform_category_deep_split(&document);
    assert_eq!(Value::Object(result.document), document);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn category_deep_split_migration_id_stays_the_shipped_value() {
    assert_eq!(
        config_migration::CATEGORY_DEEP_SPLIT_MIGRATION_ID,
        "2026-09-category-deep-split"
    );
}

#[test]
fn harness_native_rename_renames_the_legacy_block_and_keeps_every_value() {
    let document = json!({
        "categories": { "quick": { "model": "openai/gpt-6-astra" } },
        "[senpi]": {
            "categories": { "quick": { "reasoning": "high" } },
            "git_master": { "commit_footer": true },
            "telemetry": { "enabled": false },
        },
        "profiles": { "opus": { "[senpi]": { "model_profile": "opus" } } },
    });
    let result = config_migration::transform_harness_native_rename(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({
            "categories": { "quick": { "model": "openai/gpt-6-astra" } },
            "[native]": {
                "categories": { "quick": { "reasoning": "high" } },
                "git_master": { "commit_footer": true },
                "telemetry": { "enabled": false },
            },
            "profiles": { "opus": { "[native]": { "model_profile": "opus" } } },
        })
    );
    assert_eq!(
        result.diagnostics,
        vec![
            "[senpi] renamed to [native]".to_string(),
            "profiles.opus.[senpi] renamed to [native]".to_string(),
        ]
    );
}

#[test]
fn harness_native_rename_keeps_the_canonical_block_and_reports_the_drop() {
    let document = json!({
        "[native]": { "model_profile": "canonical" },
        "[senpi]": { "model_profile": "legacy" },
    });
    let result = config_migration::transform_harness_native_rename(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({ "[native]": { "model_profile": "canonical" } })
    );
    assert_eq!(
        result.diagnostics,
        vec!["[senpi] removed: [native] is already configured".to_string()]
    );
}

#[test]
fn harness_native_rename_leaves_a_clean_document_untouched() {
    let document = json!({
        "categories": { "deep-low": { "model": "openai/gpt-6-astra" } },
        "[native]": { "telemetry": { "enabled": false } },
        "[codex]": { "model_profile": "codex" },
    });
    let result = config_migration::transform_harness_native_rename(&document);
    assert_eq!(Value::Object(result.document), document);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn harness_native_rename_migration_id_stays_a_stable_dated_value() {
    assert_eq!(
        config_migration::HARNESS_NATIVE_RENAME_MIGRATION_ID,
        "2026-09-harness-native-rename"
    );
}

#[test]
fn subscription_provider_rename_canonicalizes_every_legacy_id() {
    let input = json!({
        "categories": {
            "unspecified-low": { "models": ["openai-codex/gpt-5.6-luna-fast", "anthropic/"] },
            "my-custom-lane": { "models": ["claude-sdk-oauth/", "openai/gpt-6-astra"] },
        },
        "default_model": "claude-sdk-oauth/",
        "disabled_providers": ["openai-codex"],
        "retry": { "fallback_chains": { "claude-sdk-oauth/": ["claude-sdk-oauth/", "anthropic/"] } },
        "some_unrelated_key": { "keep": "me", "nested": [1, 2, { "untouched": true }] },
    });
    let expected = json!({
        "categories": {
            "unspecified-low": { "models": ["chatgpt-subscription/gpt-5.6-luna-fast", "anthropic/"] },
            "my-custom-lane": { "models": ["anthropic-subscription/", "openai/gpt-6-astra"] },
        },
        "default_model": "anthropic-subscription/",
        "disabled_providers": ["chatgpt-subscription"],
        "retry": { "fallback_chains": { "anthropic-subscription/": ["anthropic-subscription/", "anthropic/"] } },
        "some_unrelated_key": { "keep": "me", "nested": [1, 2, { "untouched": true }] },
    });
    let result = config_migration::transform_subscription_provider_rename(&input);
    assert_eq!(Value::Object(result.document), expected);
}

#[test]
fn subscription_provider_rename_leaves_the_metered_api_key_lanes_alone() {
    let document = json!({
        "a": "openai/gpt-6-astra",
        "b": "anthropic/",
        "c": { "openai": { "x": 1 }, "anthropic": { "y": 2 } },
    });
    let result = config_migration::transform_subscription_provider_rename(&document);
    assert_eq!(Value::Object(result.document), document);
}

#[test]
fn subscription_provider_rename_carries_unrelated_keys_through_verbatim() {
    let untouched = json!({ "deep": { "nested": [1, "two", { "three": true }] }, "keep": "me" });
    let document = json!({ "deep": { "nested": [1, "two", { "three": true }] }, "keep": "me", "model": "openai-codex/gpt-6-astra" });
    let result = config_migration::transform_subscription_provider_rename(&document);
    assert_eq!(result.document["deep"], untouched["deep"]);
    assert_eq!(result.document["keep"], json!("me"));
}

#[test]
fn subscription_provider_rename_gates_on_the_legacy_ids() {
    let canonical = json!({ "default_model": "anthropic-subscription/" });
    let legacy = json!({ "default_model": "claude-sdk-oauth/" });
    assert!(!config_migration::has_legacy_subscription_provider_ids(&canonical));
    assert!(config_migration::has_legacy_subscription_provider_ids(&legacy));
}

#[test]
fn subscription_provider_rename_renames_a_legacy_id_used_as_a_key() {
    let document = json!({ "claude-sdk-oauth": { "tokenInjection": "config-dir" } });
    let result = config_migration::transform_subscription_provider_rename(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({ "anthropic-subscription": { "tokenInjection": "config-dir" } })
    );
}

#[test]
fn subscription_provider_rename_diagnostics_name_the_path_and_both_ids() {
    let document = json!({ "default_model": "openai-codex/gpt-6-astra" });
    let result = config_migration::transform_subscription_provider_rename(&document);
    assert_eq!(
        result.diagnostics,
        vec![
            "$.default_model: openai-codex/gpt-6-astra renamed to chatgpt-subscription/gpt-6-astra"
                .to_string()
        ]
    );
}

#[test]
fn subscription_provider_rename_migration_id_stays_the_recorded_value() {
    assert_eq!(
        config_migration::SUBSCRIPTION_PROVIDER_RENAME_MIGRATION_ID,
        "2026-09-subscription-provider-rename"
    );
}
