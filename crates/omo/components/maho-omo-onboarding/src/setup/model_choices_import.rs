//! The model-choice stage of `omo setup` (upstream `omo-native/bin/lib/setup-model-choices-import.js`):
//! what [`super::opencode_models`] converted, classified against the files the engine and the native
//! harness read. [`plan_model_choices`] only reads; [`apply_model_choices`] is the one writer.
//!
//! - `<agentDir>/settings.json[c]` `defaultProvider` + `defaultModel`: the engine's saved default.
//! - `~/.omo/omo.json[c]` `[native].model_profile`: headless and desktop sessions apply it.
//! - `[native].categories.<name>` / `[native].agents.<name>`: the native harness view.
//!
//! A key either file already has, in the harness block or the shared base, is never overwritten.

use super::SetupRuntime;
use super::engine_models::{LoadCatalog, ModelRegistry, load_engine_models};
use super::jsonc::parse_jsonc;
use super::jsonc_edit::insert_jsonc_member;
use super::list;
use super::opencode_models::{
    DefaultModel, convert_model_choices, has_model_choices, read_model_choices,
};
use super::opencode_providers::PlannedProvider;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A file the stage reads and may edit.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceTarget {
    pub path: PathBuf,
    pub document: Map<String, Value>,
    pub text: Option<String>,
    pub malformed: bool,
}

fn read_target(path: PathBuf) -> ChoiceTarget {
    let Ok(text) = std::fs::read_to_string(&path) else {
        return ChoiceTarget {
            path,
            document: Map::new(),
            text: None,
            malformed: false,
        };
    };
    match parse_jsonc(&text) {
        Ok(Value::Object(document)) => ChoiceTarget {
            path,
            document,
            text: Some(text),
            malformed: false,
        },
        _ => ChoiceTarget {
            path,
            document: Map::new(),
            text: Some(text),
            malformed: true,
        },
    }
}

// omo-config-core's user layer: omo.jsonc, else omo.json; a new file is omo.jsonc.
fn omo_target_path(home: &Path) -> PathBuf {
    let jsonc = home.join(".omo").join("omo.jsonc");
    let json = home.join(".omo").join("omo.json");
    if jsonc.exists() || !json.exists() {
        jsonc
    } else {
        json
    }
}

// settings.jsonc wins over settings.json.
fn settings_target_path(agent_dir: &Path) -> PathBuf {
    let jsonc = agent_dir.join("settings.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        agent_dir.join("settings.json")
    }
}

// A config still spelled `[senpi]` is written there rather than shadowed by a new block.
fn harness_block(document: &Map<String, Value>) -> &'static str {
    if !document.contains_key("[native]") && document.contains_key("[senpi]") {
        "[senpi]"
    } else {
        "[native]"
    }
}

fn lookup<'a>(document: &'a Map<String, Value>, path: &[String]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut node = document.get(first)?;
    for key in rest {
        node = node.as_object()?.get(key)?;
    }
    Some(node)
}

/// Which file an item edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetId {
    Settings,
    Omo,
}

/// One planned write.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceItem {
    pub label: String,
    /// `default`, `categories` or `agents`.
    pub kind: String,
    pub target: TargetId,
    pub path: Vec<String>,
    pub members: Vec<(String, Value)>,
    /// `pending` writes, `kept` differs from the file, `carried` is already there, `blocked`.
    pub state: &'static str,
}

fn plan_items(
    converted: &super::opencode_models::ConvertedChoices,
    omo: &ChoiceTarget,
) -> Vec<ChoiceItem> {
    let block = harness_block(&omo.document).to_owned();
    let mut items = Vec::new();
    if let Some(DefaultModel {
        provider,
        model_id,
        reference,
        ..
    }) = &converted.default_model
    {
        items.push(ChoiceItem {
            label: "default model".into(),
            kind: "default".into(),
            target: TargetId::Settings,
            path: Vec::new(),
            members: vec![
                ("defaultProvider".into(), json!(provider)),
                ("defaultModel".into(), json!(model_id)),
            ],
            state: "pending",
        });
        items.push(ChoiceItem {
            label: "model_profile".into(),
            kind: "default".into(),
            target: TargetId::Omo,
            path: vec![block.clone()],
            members: vec![("model_profile".into(), json!(reference))],
            state: "pending",
        });
    }
    for (kind, key, choices) in [
        ("categories", "category", &converted.categories),
        ("agents", "agent", &converted.agents),
    ] {
        for choice in choices {
            items.push(ChoiceItem {
                label: format!("{key} {}", choice.name),
                kind: kind.into(),
                target: TargetId::Omo,
                path: vec![block.clone(), kind.into()],
                members: vec![(choice.name.clone(), choice.entry.clone())],
                state: "pending",
            });
        }
    }
    items
}

fn classify(item: &ChoiceItem, target: &ChoiceTarget) -> &'static str {
    if target.malformed {
        return "blocked";
    }
    if let Some(container) = lookup(&target.document, &item.path)
        && !container.is_object()
    {
        return "blocked";
    }
    let existing: Vec<Option<&Value>> = item
        .members
        .iter()
        .map(|(key, _)| {
            let mut full = item.path.clone();
            full.push(key.clone());
            lookup(&target.document, &full).or_else(|| {
                if item.path.first().is_some_and(|p| p.starts_with('[')) {
                    let mut base = item.path[1..].to_vec();
                    base.push(key.clone());
                    lookup(&target.document, &base)
                } else {
                    None
                }
            })
        })
        .collect();
    if existing.iter().all(Option::is_none) {
        return "pending";
    }
    if item
        .members
        .iter()
        .zip(&existing)
        .all(|((_, value), current)| *current == Some(value))
    {
        "carried"
    } else {
        "kept"
    }
}

/// The model-choice stage's plan.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelChoicePlan {
    pub notices: Vec<String>,
    pub present: bool,
    pub settings: Option<ChoiceTarget>,
    pub omo: Option<ChoiceTarget>,
    pub items: Vec<ChoiceItem>,
    pub dropped: Vec<String>,
    pub default_model: Option<DefaultModel>,
    pub pending: usize,
    /// What the apply wrote (`plan.carried`), for the counts.
    pub carried: Vec<String>,
}

impl ModelChoicePlan {
    pub fn target(&self, id: TargetId) -> Option<&ChoiceTarget> {
        match id {
            TargetId::Settings => self.settings.as_ref(),
            TargetId::Omo => self.omo.as_ref(),
        }
    }
}

fn labels(plan: &ModelChoicePlan, state: &str) -> Vec<String> {
    plan.items
        .iter()
        .filter(|i| i.state == state)
        .map(|i| i.label.clone())
        .collect()
}

pub fn model_choice_plan_lines(plan: &ModelChoicePlan) -> Vec<String> {
    vec![
        list("planned-model-choices", &labels(plan, "pending")),
        list("model-choices-skipped-existing", &labels(plan, "kept")),
        list("model-choices-already-carried", &labels(plan, "carried")),
        format!("model-choices-not-carried: {}", plan.dropped.len()),
    ]
}

pub fn model_choice_counts(plan: &ModelChoicePlan) -> Vec<String> {
    vec![
        list("model-choices-carried", &plan.carried),
        list("model-choices-skipped-existing", &labels(plan, "kept")),
        format!("model-choices-not-carried: {}", plan.dropped.len()),
    ]
}

pub fn model_choice_question(plan: &ModelChoicePlan) -> String {
    let mut targets: Vec<String> = Vec::new();
    for item in plan.items.iter().filter(|i| i.state == "pending") {
        if let Some(target) = plan.target(item.target) {
            let path = target.path.display().to_string();
            if !targets.contains(&path) {
                targets.push(path);
            }
        }
    }
    format!(
        "Write {} model choice(s) into {}? [y/N] ",
        plan.pending,
        targets.join(" and ")
    )
}

/// What the stage reads: the runtime, the agent dir, the providers this run adds, and the opencode
/// OAuth logins.
pub struct ModelChoiceStage<'a> {
    pub runtime: &'a SetupRuntime,
    pub agent_dir: &'a Path,
    pub custom_providers: &'a [PlannedProvider],
    pub oauth_providers: &'a HashSet<String>,
    /// Upstream `stage.loadRegistry`: replaces the engine model list.
    pub load_registry: Option<Arc<dyn Fn() -> Result<ModelRegistry, String> + Send + Sync>>,
}

/// Read-only. `custom_providers` are validated as if models.json had them.
pub fn plan_model_choices(stage: &ModelChoiceStage<'_>) -> ModelChoicePlan {
    let home = Path::new(&stage.runtime.home);
    let omo = read_target(omo_target_path(home));
    let raw = read_model_choices(stage.runtime, omo.document.get("[opencode]"));
    let empty = ModelChoicePlan {
        notices: raw.notices.clone(),
        present: false,
        settings: None,
        omo: None,
        items: Vec::new(),
        dropped: Vec::new(),
        default_model: None,
        pending: 0,
        carried: Vec::new(),
    };
    if !has_model_choices(&raw) {
        return empty;
    }
    let catalog: Option<&LoadCatalog> = stage.runtime.load_model_catalog.as_ref();
    let registry = match &stage.load_registry {
        Some(load) => load(),
        None => load_engine_models(stage.agent_dir, stage.custom_providers, catalog),
    };
    let registry = match registry {
        Ok(registry) => registry,
        Err(error) => {
            let mut notices = raw.notices.clone();
            notices.push(format!("WARN senpi: could not read the engine's model list ({error}); model choices were not carried"));
            return ModelChoicePlan { notices, ..empty };
        }
    };
    let converted = convert_model_choices(&raw, &registry, stage.oauth_providers);
    let settings = read_target(settings_target_path(stage.agent_dir));
    let mut items = plan_items(&converted, &omo);
    for item in &mut items {
        item.state = classify(
            item,
            if item.target == TargetId::Settings {
                &settings
            } else {
                &omo
            },
        );
    }
    let mut notices = raw.notices.clone();
    // Every reason a choice was not written: a blocked target, then each dropped choice.
    for (id, target) in [(TargetId::Settings, &settings), (TargetId::Omo, &omo)] {
        let blocked: Vec<&str> = items
            .iter()
            .filter(|i| i.state == "blocked" && i.target == id)
            .map(|i| i.label.as_str())
            .collect();
        if !blocked.is_empty() {
            notices.push(format!(
                "WARN senpi: {} is malformed or has a non-object block; these model choices were not written: {}",
                target.path.display(),
                blocked.join(", ")
            ));
        }
    }
    for reason in &converted.dropped {
        notices.push(format!("model choice not carried: {reason}"));
    }
    let pending = items.iter().filter(|i| i.state == "pending").count();
    ModelChoicePlan {
        notices,
        present: true,
        settings: Some(settings),
        omo: Some(omo),
        items,
        dropped: converted.dropped,
        default_model: converted.default_model,
        pending,
        carried: Vec::new(),
    }
}

fn write_text(target: &ChoiceTarget, text: &str) -> std::io::Result<()> {
    if let Some(parent) = target.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if target.text.is_some() {
        std::fs::copy(
            &target.path,
            format!("{}.bak-{}", target.path.display(), super::timestamp()),
        )?;
    }
    // A config symlinked from a dotfiles checkout is written through the link.
    let destination = if target.text.is_some() {
        std::fs::canonicalize(&target.path)?
    } else {
        target.path.clone()
    };
    let temporary = PathBuf::from(format!(
        "{}.tmp-{}",
        destination.display(),
        std::process::id()
    ));
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&temporary, text)?;
        // The edited file keeps its own permissions: a 0600 config must not come back world-readable.
        if target.text.is_some() {
            std::fs::set_permissions(&temporary, std::fs::metadata(&destination)?.permissions())?;
        } else {
            super::set_private(&temporary)?;
        }
        std::fs::rename(&temporary, &destination)
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

// Every edit is an insertion into the file's own text; a file the edit cannot reproduce exactly is
// left untouched and reported.
fn write_target(
    target: &ChoiceTarget,
    items: &[&ChoiceItem],
    report: &mut dyn FnMut(&str),
) -> Vec<String> {
    let written = (|| -> Result<(), String> {
        let mut text = target.text.clone().unwrap_or_else(|| "{}\n".into());
        for item in items {
            for (key, value) in &item.members {
                text = insert_jsonc_member(&text, &item.path, key, value)?;
            }
        }
        write_text(target, &text).map_err(|e| e.to_string())
    })();
    match written {
        Ok(()) => items.iter().map(|i| i.label.clone()).collect(),
        Err(error) => {
            report(&format!(
                "WARN senpi: could not edit {} safely ({error}); these model choices were not written: {}",
                target.path.display(),
                items
                    .iter()
                    .map(|i| i.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            Vec::new()
        }
    }
}

/// The stage's one writer; records what it wrote in `plan.carried`.
pub fn apply_model_choices(plan: &mut ModelChoicePlan, report: &mut dyn FnMut(&str)) {
    let mut carried = Vec::new();
    for id in [TargetId::Settings, TargetId::Omo] {
        let pending: Vec<&ChoiceItem> = plan
            .items
            .iter()
            .filter(|i| i.state == "pending" && i.target == id)
            .collect();
        if pending.is_empty() {
            continue;
        }
        if let Some(target) = plan.target(id) {
            carried.extend(write_target(target, &pending, report));
        }
    }
    plan.carried = carried;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::engine_models::CatalogProvider;
    use std::collections::BTreeMap;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn runtime(home: &Path) -> SetupRuntime {
        let mut runtime = SetupRuntime::new(home.to_string_lossy(), BTreeMap::new());
        runtime.load_model_catalog = Some(Arc::new(|| -> Result<Vec<CatalogProvider>, String> {
            Ok(vec![
                CatalogProvider {
                    id: "openai".into(),
                    models: vec!["gpt-5".into()],
                    refreshes: false,
                },
                CatalogProvider {
                    id: "opencode-go".into(),
                    models: vec!["glm-5.2".into()],
                    refreshes: false,
                },
            ])
        }));
        runtime
    }

    #[test]
    fn choices_are_written_into_settings_and_omo_keeping_existing_keys_and_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let agent = home.join(".omo/agent");
        write(
            &home.join(".config/opencode/opencode.json"),
            r#"{"model":"openai/gpt-5"}"#,
        );
        write(
            &home.join(".config/opencode/oh-my-openagent.json"),
            r#"{"categories":{"quick":{"model":"opencode-go/glm-5.2"},"deep":{"model":"openai/gpt-5"}}}"#,
        );
        write(
            &home.join(".omo/omo.jsonc"),
            "{\n  // mine\n  \"[native]\": { \"categories\": { \"deep-low\": { \"model\": \"x/y\" } } }\n}\n",
        );
        let oauth = HashSet::new();
        let stage = ModelChoiceStage {
            runtime: &runtime(&home),
            agent_dir: &agent,
            custom_providers: &[],
            oauth_providers: &oauth,
            load_registry: None,
        };
        let mut plan = plan_model_choices(&stage);
        assert!(plan.present);
        assert_eq!(
            model_choice_plan_lines(&plan),
            vec![
                "planned-model-choices: default model, model_profile, category quick",
                "model-choices-skipped-existing: category deep-low",
                "model-choices-already-carried: none",
                "model-choices-not-carried: 0",
            ]
        );
        let mut reports = Vec::new();
        apply_model_choices(&mut plan, &mut |line| reports.push(line.to_owned()));
        assert!(reports.is_empty(), "{reports:?}");
        assert_eq!(
            plan.carried,
            vec!["default model", "model_profile", "category quick"]
        );
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent.join("settings.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(
            settings,
            json!({"defaultProvider": "openai", "defaultModel": "gpt-5"})
        );
        let omo_text = std::fs::read_to_string(home.join(".omo/omo.jsonc")).expect("read");
        assert!(omo_text.contains("// mine"));
        assert_eq!(
            parse_jsonc(&omo_text).expect("parse"),
            json!({"[native]": {"categories": {"deep-low": {"model": "x/y"}, "quick": {"model": "opencode-go/glm-5.2"}}, "model_profile": "openai/gpt-5"}})
        );
        let again = plan_model_choices(&stage);
        assert_eq!(again.pending, 0);
        assert_eq!(
            labels(&again, "carried"),
            vec!["default model", "model_profile", "category quick"]
        );
    }

    #[test]
    fn without_choices_the_stage_is_absent_and_a_failing_catalog_is_noticed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let oauth = HashSet::new();
        let rt = runtime(&home);
        let stage = ModelChoiceStage {
            runtime: &rt,
            agent_dir: &home,
            custom_providers: &[],
            oauth_providers: &oauth,
            load_registry: None,
        };
        assert!(!plan_model_choices(&stage).present);
        write(
            &home.join(".config/opencode/opencode.json"),
            r#"{"model":"openai/gpt-5"}"#,
        );
        let failing = ModelChoiceStage {
            load_registry: Some(Arc::new(|| -> Result<ModelRegistry, String> {
                Err("boom".into())
            })),
            ..stage
        };
        let plan = plan_model_choices(&failing);
        assert!(!plan.present);
        assert_eq!(
            plan.notices,
            vec![
                "WARN senpi: could not read the engine's model list (boom); model choices were not carried"
            ]
        );
    }
}
