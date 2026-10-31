//! One `{id, kind, state, detail?}` row per thing a setup source found, built from the stage plan
//! (upstream `omo-native/bin/lib/setup-stream-items.js`). Rows travel over RPC, so they carry ids
//! and states only: never a key.

use super::assets_import::AssetPlan;
use super::credentials::CredentialPlan;
use super::detect::Inventory;
use super::model_choices_import::ModelChoicePlan;
use super::providers_import::ProviderPlan;
use crate::scan_contract::OnboardingScanItem;

fn row(id: &str, kind: &str, state: &str, detail: Option<String>) -> OnboardingScanItem {
    OnboardingScanItem {
        id: id.to_owned(),
        kind: kind.to_owned(),
        state: state.to_owned(),
        detail,
        project: None,
    }
}

fn rows(ids: &[String], kind: &str, state: &str) -> Vec<OnboardingScanItem> {
    ids.iter().map(|id| row(id, kind, state, None)).collect()
}

pub fn detect_items(inventory: &Inventory) -> Vec<OnboardingScanItem> {
    inventory
        .harnesses
        .iter()
        .filter(|h| h.installed)
        .map(|h| {
            row(
                &h.id,
                "harness",
                "found",
                Some(format!(
                    "{} login(s), {} importable",
                    h.entry_count, h.importable_count
                )),
            )
        })
        .collect()
}

pub fn provider_items(plan: &ProviderPlan) -> Vec<OnboardingScanItem> {
    let result = &plan.result;
    let mut items: Vec<OnboardingScanItem> = result
        .added
        .iter()
        .map(|p| {
            row(
                &p.id,
                "provider",
                "import",
                Some(format!("{} model(s)", p.model_ids().len())),
            )
        })
        .collect();
    items.extend(rows(&result.skipped_existing, "provider", "exists"));
    items.extend(rows(&result.blocked, "provider", "blocked"));
    items.extend(rows(&plan.skipped, "provider", "skipped"));
    items
}

pub fn credential_items(plan: &CredentialPlan) -> Vec<OnboardingScanItem> {
    if plan.malformed {
        return Vec::new();
    }
    let result = &plan.result;
    let mut items: Vec<OnboardingScanItem> = result
        .additions
        .iter()
        .map(|i| {
            row(
                &i.provider,
                "api-key",
                "import",
                Some(format!("from {}", i.source)),
            )
        })
        .collect();
    items.extend(rows(&result.skipped_existing, "api-key", "exists"));
    items.extend(plan.guidance.logins.iter().map(|login| {
        row(
            &login.provider,
            "oauth",
            login.state,
            login.target.as_ref().map(|t| format!("/login {t}")),
        )
    }));
    items.extend(rows(&result.skipped_unmapped, "api-key", "unmapped"));
    items
}

pub fn asset_items(plan: &AssetPlan) -> Vec<OnboardingScanItem> {
    let result = &plan.result;
    let mut items: Vec<OnboardingScanItem> = result
        .servers
        .added
        .iter()
        .map(|s| row(&s.name, "mcp", "import", None))
        .collect();
    items.extend(rows(&result.servers.skipped_existing, "mcp", "exists"));
    items.extend(rows(&result.servers.blocked, "mcp", "blocked"));
    items.extend(
        result
            .skills
            .added
            .iter()
            .map(|s| row(&s.name, "skill", "import", None)),
    );
    items.extend(rows(&result.skills.skipped_existing, "skill", "exists"));
    items.extend(rows(&result.skills.skipped_bundled, "skill", "bundled"));
    items
}

pub fn model_choice_items(plan: &ModelChoicePlan) -> Vec<OnboardingScanItem> {
    plan.items
        .iter()
        .map(|item| {
            let state = match item.state {
                "pending" => "import",
                "kept" => "exists",
                other => other,
            };
            row(&item.label, &item.kind, state, None)
        })
        .collect()
}
