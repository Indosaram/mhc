//! Port of senpi `components/config-startup/opencode-routing-notice.ts`.
//!
//! Reports, once, the OpenCode edition's agent and category model settings Native does not use,
//! with the mapping `omo setup` applies. It writes nothing but its `_migrations` marker: the legacy
//! migration deliberately never copies those settings, and `omo setup` carries them with the user's
//! consent. Gated on content like the other in-place plans, so a config with nothing to report gets
//! no write and no marker.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use config_migration::{
    ConfigMigrationTransformResult, LegacyConfigMigrationPlan, LegacyConfigMigrationTransform,
};
use maho_omo_onboarding::setup::SetupRuntime;
use maho_omo_onboarding::setup::opencode_models::{
    EditionRouting, open_code_routing_gap, open_code_routing_notice, read_edition_routing,
};
use omo_config_core::{LoadedMigrationSource, MigrationMode};
use serde_json::{Map, Value};

pub const OPENCODE_ROUTING_NOTICE_MIGRATION_ID: &str = "2026-09-opencode-routing-notice";

pub struct RoutingNoticePlanOptions<'a> {
    pub environment: &'a BTreeMap<String, String>,
    pub home_dir: &'a str,
    pub target_path: String,
}

fn reported(document: &Value) -> bool {
    document
        .get("_migrations")
        .and_then(Value::as_array)
        .is_some_and(|markers| {
            markers
                .iter()
                .any(|marker| marker.as_str() == Some(OPENCODE_ROUTING_NOTICE_MIGRATION_ID))
        })
}

fn document_of(source: Option<&LoadedMigrationSource>) -> Value {
    source
        .map(|source| source.value.clone())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Reads the edition once per plan, so `should_run` and `transform` see the same routing.
pub fn create_open_code_routing_notice_plan(
    options: &RoutingNoticePlanOptions<'_>,
) -> LegacyConfigMigrationPlan {
    let edition: Arc<EditionRouting> = Arc::new(read_edition_routing(&SetupRuntime::new(
        options.home_dir.to_owned(),
        options.environment.clone(),
    )));
    let should_run = {
        let edition = Arc::clone(&edition);
        Rc::new(move |target: &Value| {
            !reported(target) && !open_code_routing_gap(target, &edition).is_empty()
        }) as Rc<dyn Fn(&Value) -> bool>
    };
    let transform: LegacyConfigMigrationTransform = {
        let edition = Arc::clone(&edition);
        Rc::new(move |loaded: &[LoadedMigrationSource]| {
            let document = match document_of(loaded.first()) {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            let gap = open_code_routing_gap(&Value::Object(document.clone()), &edition);
            Ok(ConfigMigrationTransformResult {
                diagnostics: vec![open_code_routing_notice(&gap)],
                document,
            })
        })
    };
    LegacyConfigMigrationPlan {
        id: OPENCODE_ROUTING_NOTICE_MIGRATION_ID.to_string(),
        inspect: Rc::clone(&transform),
        mode: MigrationMode::ReplaceTarget,
        should_run: Some(should_run),
        sources: Vec::new(),
        target_path: options.target_path.clone(),
        transform,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_edition() -> EditionRouting {
        EditionRouting {
            agents: Default::default(),
            categories: Default::default(),
            notices: Vec::new(),
        }
    }

    fn plan(target: &str) -> LegacyConfigMigrationPlan {
        create_open_code_routing_notice_plan(&RoutingNoticePlanOptions {
            environment: &BTreeMap::new(),
            home_dir: "/nonexistent-home-for-routing-notice",
            target_path: target.to_owned(),
        })
    }

    #[test]
    fn a_config_with_nothing_to_report_is_not_touched() {
        let plan = plan("/tmp/omo.jsonc");
        let should_run = plan.should_run.expect("content gate");
        assert!(!should_run(&Value::Object(Map::new())));
    }

    #[test]
    fn the_marker_stops_a_second_report() {
        let plan = plan("/tmp/omo.jsonc");
        let should_run = plan.should_run.expect("content gate");
        assert!(!should_run(&serde_json::json!({
            "_migrations": [OPENCODE_ROUTING_NOTICE_MIGRATION_ID]
        })));
    }

    #[test]
    fn the_plan_reports_through_the_transform_without_rewriting_the_document() {
        let plan = plan("/tmp/omo.jsonc");
        assert_eq!(plan.id, OPENCODE_ROUTING_NOTICE_MIGRATION_ID);
        assert!(matches!(plan.mode, MigrationMode::ReplaceTarget));
        assert!(plan.sources.is_empty());
        let source = LoadedMigrationSource {
            backup_path: None,
            path: "/tmp/omo.jsonc".into(),
            value: serde_json::json!({"categories": {"quick": {"model": "openai/gpt-5"}}}),
        };
        let result = (plan.transform)(&[source]).expect("transform");
        assert_eq!(
            Value::Object(result.document),
            serde_json::json!({"categories": {"quick": {"model": "openai/gpt-5"}}})
        );
        assert_eq!(result.diagnostics.len(), 1);
        assert!(result.diagnostics[0].starts_with(
            "OpenCode edition model settings Native does not use; run omo setup to carry them over"
        ));
    }

    #[test]
    fn a_non_object_target_becomes_an_empty_document() {
        let plan = plan("/tmp/omo.jsonc");
        let source = LoadedMigrationSource {
            backup_path: None,
            path: "/tmp/omo.jsonc".into(),
            value: Value::Null,
        };
        let result = (plan.transform)(&[source]).expect("transform");
        assert_eq!(result.document, Map::new());
    }

    #[test]
    fn the_edition_reader_is_reachable_for_the_plan() {
        let runtime = SetupRuntime::new("/nonexistent-home", BTreeMap::new());
        let edition = read_edition_routing(&runtime);
        assert!(edition.categories.is_empty());
        assert!(empty_edition().agents.is_empty());
    }
}
