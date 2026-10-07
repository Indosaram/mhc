//! Kibitzer entry records and renderers (latest `kibitzer/notice.ts`).
//!
//! The nudge is the only user-visible trace of a Kibitzer verdict; the gate and unavailable
//! records report a failed/absent judge. Every renderer is fail-closed: a record that does not
//! match the producer contract draws nothing.

use serde::{Deserialize, Serialize};

use memory_core::recall::{NUDGE_HINT_MAX_CHARS, is_valid_hint};
use memory_core::sync::redact::contains_secret_like_material;

use crate::worker::completion_renderers::ResolveEntryTheme;
use crate::worker::entry_renderers::{
    NoticeComponent, NoticeExtraLine, NoticeSpec, join_fields,
};

pub const NUDGED_ENTRY_TYPE: &str = "omo-kibitzer:nudged";
pub const GATE_ENTRY_TYPE: &str = "omo-kibitzer:gate";
pub const UNAVAILABLE_ENTRY_TYPE: &str = "omo-kibitzer:unavailable";
pub const GATE_REASON_MAX_CHARS: usize = 160;
pub const UNAVAILABLE_CATEGORY_MAX_CHARS: usize = 128;
pub const UNAVAILABLE_PROVIDER_MAX_CHARS: usize = 64;
pub const GATE_MODEL_MAX_CHARS: usize = 128;
pub const UNAVAILABLE_PROVIDER_MAX_COUNT: usize = 16;

/// One delivered nudge's user-facing projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KibitzerNudge {
    pub path: String,
    pub hint: String,
}

/// `KibitzerNudgedRecord`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KibitzerNudgedRecord {
    pub version: u32,
    pub nudges: Vec<KibitzerNudge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// `KibitzerGateRecord` (only the fields the renderer draws are modelled).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KibitzerGateRecord {
    pub version: u32,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub candidate_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<i64>,
}

/// `KibitzerUnavailableRecord`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KibitzerUnavailableRecord {
    pub version: u32,
    pub category: String,
    pub cause: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_providers: Option<Vec<String>>,
}

fn normalize(value: &str) -> String {
    senpi_task::renderer_text::normalize_renderer_text(value)
}

fn normalize_nudge(nudge: &KibitzerNudge) -> Option<KibitzerNudge> {
    if nudge.hint.chars().count() > NUDGE_HINT_MAX_CHARS || !is_valid_hint(&nudge.hint) {
        return None;
    }
    let path = normalize(&nudge.path);
    let hint = normalize(&nudge.hint);
    if path.is_empty() || hint.is_empty() {
        return None;
    }
    Some(KibitzerNudge { path, hint })
}

/// `renderKibitzerNudgedEntry`: fixed `Kibitzer !` title, one line per hint, then the paths.
pub fn kibitzer_nudged_spec(record: &KibitzerNudgedRecord) -> Option<NoticeSpec> {
    if record.version != 1 || record.nudges.is_empty() {
        return None;
    }
    let mut nudges = Vec::new();
    for nudge in &record.nudges {
        nudges.push(normalize_nudge(nudge)?);
    }
    let first = nudges.first()?;
    let mut extra: Vec<NoticeExtraLine> = nudges
        .iter()
        .skip(1)
        .map(|nudge| NoticeExtraLine { text: format!("recalled memory: {}", nudge.hint), tone: Some("dim".into()) })
        .collect();
    extra.extend(nudges.iter().map(|nudge| NoticeExtraLine { text: nudge.path.clone(), tone: Some("dim".into()) }));
    Some(NoticeSpec {
        glyph: "\u{2726}".into(),
        title: "Kibitzer !".into(),
        tone: "accent".into(),
        why: format!("recalled memory: {}", first.hint),
        extra,
        detail: Some("Kibitzer surfaced this from stored memory; it is a hint, not current state.".into()),
    })
}

fn valid_gate_reason(value: &str) -> Option<String> {
    let normalized = normalize(value);
    if normalized.is_empty() || normalized.chars().count() > GATE_REASON_MAX_CHARS || contains_secret_like_material(value) {
        return None;
    }
    if value.contains('\r') || value.contains('\n') {
        return None;
    }
    Some(normalized)
}

fn valid_gate_field(value: &str, max_chars: usize) -> Option<String> {
    if value.contains('\r') || value.contains('\n') || contains_secret_like_material(value) {
        return None;
    }
    let normalized = normalize(value);
    if normalized.is_empty() || normalized.chars().count() > max_chars {
        None
    } else {
        Some(normalized)
    }
}

fn valid_run_id(value: &str) -> Option<String> {
    let ok = !value.is_empty()
        && value.chars().count() <= 64
        && value.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    ok.then(|| value.to_string())
}

/// `renderKibitzerGateEntry`: `skipped`/`failed` only, with the consecutive-failure count.
pub fn kibitzer_gate_spec(record: &KibitzerGateRecord) -> Option<NoticeSpec> {
    if record.version != 1 || record.status == "dropped" {
        return None;
    }
    if record.status != "skipped" && record.status != "failed" {
        return None;
    }
    if record.candidate_count < 0 {
        return None;
    }
    let consecutive = record.consecutive_failures.filter(|count| *count >= 1)?;
    let cause = record.cause.as_deref().map(normalize).filter(|cause| !cause.is_empty());
    let reason = record.reason.as_deref().and_then(valid_gate_reason);
    let run_id = record.run_id.as_deref().and_then(valid_run_id);
    let model = record.model.as_deref().and_then(|model| valid_gate_field(model, GATE_MODEL_MAX_CHARS));
    let category = record.category.as_deref().and_then(|category| valid_gate_field(category, UNAVAILABLE_CATEGORY_MAX_CHARS));
    let fix = match &category {
        None => "check Kibitzer model/provider settings".to_string(),
        Some(category) => format!("set categories.{category}.model (or memory.recall.category) in omo.json to a model that answers"),
    };
    let mut extra: Vec<NoticeExtraLine> = Vec::new();
    if let Some(reason) = reason {
        extra.push(NoticeExtraLine { text: reason, tone: Some("dim".into()) });
    }
    if let Some(run_id) = run_id {
        extra.push(NoticeExtraLine { text: format!("run {run_id}"), tone: Some("dim".into()) });
    }
    if let Some(model) = model {
        let suffix = category.as_deref().map(|category| format!(" (memory recall category \"{category}\")")).unwrap_or_default();
        extra.push(NoticeExtraLine { text: format!("last failed model: {model}{suffix}"), tone: Some("dim".into()) });
    }
    extra.push(NoticeExtraLine { text: format!("after {consecutive} consecutive failures; {fix}"), tone: Some("dim".into()) });
    let skipped = record.status == "skipped";
    Some(NoticeSpec {
        glyph: if skipped { "\u{26a0}".into() } else { "\u{2717}".to_string() },
        title: join_fields(&[Some(if skipped { "Kibitzer gate skipped" } else { "Kibitzer gate failed" }), cause.as_deref()]),
        tone: if skipped { "warning".into() } else { "error".into() },
        why: if skipped {
            "Kibitzer could not judge the recalled memory candidates for the previous turn.".into()
        } else {
            "Kibitzer failed while judging the recalled memory candidates for the previous turn.".into()
        },
        extra,
        detail: None,
    })
}

fn unavailable_providers(value: Option<&Vec<String>>) -> Option<Vec<String>> {
    let Some(providers) = value else {
        return Some(Vec::new());
    };
    let mut result = Vec::new();
    for entry in providers {
        let normalized = normalize(entry);
        let bounded: String = normalized.chars().take(UNAVAILABLE_PROVIDER_MAX_CHARS).collect();
        if !bounded.is_empty() {
            result.push(bounded);
        }
        if result.len() >= UNAVAILABLE_PROVIDER_MAX_COUNT {
            break;
        }
    }
    Some(result)
}

/// `renderKibitzerUnavailableEntry`: the once-per-session category-unavailable notice.
pub fn kibitzer_unavailable_spec(record: &KibitzerUnavailableRecord) -> Option<NoticeSpec> {
    if record.version != 1 || record.cause != "category_unavailable" && record.cause != "beyond_category" {
        return None;
    }
    let category: String = normalize(&record.category).chars().take(UNAVAILABLE_CATEGORY_MAX_CHARS).collect();
    if category.is_empty() {
        return None;
    }
    let providers = unavailable_providers(record.missing_providers.as_ref())?;
    let why = if record.cause == "category_unavailable" {
        format!("No connected provider serves the \"{category}\" category chain, so recalled-memory judging is off; it resumes by itself once one is connected.")
    } else {
        format!("The \"{category}\" category chain has no connected model and Kibitzer never falls back beyond the category, so recalled-memory judging is off.")
    };
    let extra = if providers.is_empty() {
        vec![NoticeExtraLine { text: format!("pin categories.{category}.model (or memory.recall.category) in omo.json to a connected model"), tone: Some("dim".into()) }]
    } else {
        vec![
            NoticeExtraLine { text: format!("connect one of: {} (run /login <provider>)", providers.join(", ")), tone: Some("dim".into()) },
            NoticeExtraLine { text: format!("or pin categories.{category}.model (or memory.recall.category) in omo.json"), tone: Some("dim".into()) },
        ]
    };
    Some(NoticeSpec {
        glyph: "\u{26a0}".into(),
        title: join_fields(&[Some("Kibitzer unavailable"), Some(&category)]),
        tone: "warning".into(),
        why,
        extra,
        detail: Some("Kibitzer is pinned to the memory.recall.category chain on purpose: an advisor reading the live transcript must never land on a frontier-priced model outside it.".into()),
    })
}

/// Registers the three Kibitzer entry renderers.
pub fn register_kibitzer_notice_renderers(api: &mut maho_ext_api::ExtensionApi, theme: ResolveEntryTheme) {
    register_spec_renderer(api, NUDGED_ENTRY_TYPE, theme.clone(), |data| serde_json::from_value::<KibitzerNudgedRecord>(data).ok().and_then(|record| kibitzer_nudged_spec(&record)));
    register_spec_renderer(api, GATE_ENTRY_TYPE, theme.clone(), |data| serde_json::from_value::<KibitzerGateRecord>(data).ok().and_then(|record| kibitzer_gate_spec(&record)));
    register_spec_renderer(api, UNAVAILABLE_ENTRY_TYPE, theme, |data| serde_json::from_value::<KibitzerUnavailableRecord>(data).ok().and_then(|record| kibitzer_unavailable_spec(&record)));
}

fn register_spec_renderer(
    api: &mut maho_ext_api::ExtensionApi,
    entry_type: &str,
    theme: ResolveEntryTheme,
    spec_of: impl Fn(serde_json::Value) -> Option<NoticeSpec> + Send + Sync + 'static,
) {
    api.register_entry_renderer(
        entry_type,
        std::sync::Arc::new(move |entry, options, native_theme| {
            let data = entry.data.get("data")?.clone();
            let spec = spec_of(data)?;
            Some(Box::new(NoticeComponent { spec, expanded: options.expanded, theme: theme(native_theme) }) as Box<dyn maho_ext_api::Component>)
        }),
        Default::default(),
    );
}

#[cfg(test)]
#[path = "kibitzer_notice_tests.rs"]
mod tests;
