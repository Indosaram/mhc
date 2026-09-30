//! Port of senpi packages/ai/src/cursor/model-capabilities.ts.
// ported by todo 13

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::types::ModelThinkingLevel;

pub type CursorCapabilityEvidence = &'static str;
pub type CursorLevelEncoding = &'static str;
pub type CursorParameterId = &'static str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorLevelSpec {
    pub value: String,
    pub encoding: CursorLevelEncoding,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CursorModelCapability {
    pub catalog_key: Option<String>,
    pub evidence: CursorCapabilityEvidence,
    pub window: f64,
    pub max_window: Option<f64>,
    pub parameter_order: Vec<CursorParameterId>,
    pub default_context: Option<String>,
    /// Context token matching `window`; cursor truncates to whatever this asks for.
    pub request_context: Option<String>,
    pub levels: BTreeMap<ModelThinkingLevel, CursorLevelSpec>,
}

const P: CursorLevelEncoding = "parameters";
const V: CursorLevelEncoding = "variant-id";

fn level_token_to_thinking_level(value: &str) -> ModelThinkingLevel {
    match value {
        "none" | "off" => ModelThinkingLevel::Off,
        "minimal" => ModelThinkingLevel::Minimal,
        "low" => ModelThinkingLevel::Low,
        "medium" => ModelThinkingLevel::Medium,
        "high" => ModelThinkingLevel::High,
        "xhigh" | "extra-high" => ModelThinkingLevel::Xhigh,
        "max" => ModelThinkingLevel::Max,
        other => panic!("unknown cursor level token: {other}"),
    }
}

fn ladder(values: &[&str], encoding: CursorLevelEncoding) -> BTreeMap<ModelThinkingLevel, CursorLevelSpec> {
    let mut out = BTreeMap::new();
    for value in values {
        out.insert(
            level_token_to_thinking_level(value),
            CursorLevelSpec { value: (*value).to_owned(), encoding },
        );
    }
    out
}

const CLAUDE_ORDER: &[CursorParameterId] = &["thinking", "context", "effort"];
const GPT_ORDER: &[CursorParameterId] = &["context", "reasoning", "fast"];

fn claude(window: f64, max_window: f64, levels: &[&str], default_context: &str) -> CursorModelCapability {
    CursorModelCapability {
        catalog_key: None,
        evidence: "available-models",
        window,
        max_window: Some(max_window),
        parameter_order: CLAUDE_ORDER.to_vec(),
        default_context: Some(default_context.to_owned()),
        request_context: Some(if window >= 1_000_000.0 { "1m".to_owned() } else { default_context.to_owned() }),
        levels: ladder(levels, P),
    }
}

fn gpt_full(levels: &[&str], order: &[CursorParameterId], window: f64, max_window: f64) -> CursorModelCapability {
    let default_context = "272k".to_owned();
    let request_context =
        if order.contains(&"context") && window >= 1_000_000.0 { Some("1m".to_owned()) } else { None };
    CursorModelCapability {
        catalog_key: None,
        evidence: "available-models",
        window,
        max_window: Some(max_window),
        parameter_order: order.to_vec(),
        default_context: Some(default_context),
        request_context,
        levels: ladder(levels, P),
    }
}

fn gpt(levels: &[&str]) -> CursorModelCapability {
    gpt_full(levels, GPT_ORDER, 400_000.0, 400_000.0)
}

/// Static cursor capability table. Values come from the live
/// aiserver.v1 AvailableModels capture of 2026-08-18 (evidence:
/// local-ignore/qa-evidence/20260818-cursor-reasoning-levels/available-models-catalog.json).
/// `GetUsableModels` carries no window or parameter field, so this table is the
/// authoritative source; see .omo/plans/cursor-reasoning-levels.md §3.1/§6.
pub static CURSOR_MODEL_CAPABILITIES: LazyLock<BTreeMap<&'static str, CursorModelCapability>> = LazyLock::new(|| {
    let mut m = BTreeMap::new();
    m.insert("claude-fable-5", claude(1_000_000.0, 1_000_000.0, &["low", "medium", "high", "xhigh", "max"], "300k"));
    m.insert("claude-sonnet-5", claude(1_000_000.0, 1_000_000.0, &["low", "medium", "high", "xhigh", "max"], "300k"));
    m.insert("claude-opus-4-7", claude(1_000_000.0, 1_000_000.0, &["low", "medium", "high", "xhigh", "max"], "300k"));
    m.insert("claude-opus-4-8", claude(1_000_000.0, 1_000_000.0, &["low", "medium", "high", "xhigh", "max"], "300k"));
    m.insert("claude-opus-5", claude(1_000_000.0, 1_000_000.0, &["low", "medium", "high", "xhigh", "max"], "300k"));
    m.insert("claude-4.6-opus", claude(1_000_000.0, 1_000_000.0, &["high", "max"], "200k"));
    m.insert("claude-4.6-sonnet", claude(1_000_000.0, 1_000_000.0, &["medium"], "200k"));
    m.insert(
        "claude-4.5-opus",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 200_000.0,
            max_window: None,
            parameter_order: vec!["thinking"],
            default_context: None,
            request_context: None,
            levels: BTreeMap::from([(
                ModelThinkingLevel::High,
                CursorLevelSpec { value: "high".to_owned(), encoding: V },
            )]),
        },
    );
    m.insert(
        "gpt-5.6-sol",
        gpt_full(&["none", "low", "medium", "high", "xhigh", "max"], GPT_ORDER, 1_000_000.0, 1_000_000.0),
    );
    m.insert(
        "gpt-5.6-luna",
        gpt_full(&["none", "low", "medium", "high", "xhigh", "max"], GPT_ORDER, 1_000_000.0, 1_000_000.0),
    );
    m.insert(
        "gpt-5.6-terra",
        gpt_full(&["none", "low", "medium", "high", "xhigh", "max"], GPT_ORDER, 272_000.0, 272_000.0),
    );
    m.insert("gpt-5.5", {
        let mut cap = gpt_full(&["none", "low", "medium", "high"], GPT_ORDER, 1_000_000.0, 1_000_000.0);
        let mut levels = ladder(&["none", "low", "medium", "high"], P);
        levels.insert(ModelThinkingLevel::Xhigh, CursorLevelSpec { value: "extra-high".to_owned(), encoding: P });
        cap.levels = levels;
        cap
    });
    m.insert("gpt-5.3-codex", {
        let mut cap = gpt(&[]);
        cap.parameter_order = vec!["reasoning", "fast"];
        cap.max_window = None;
        cap.default_context = None;
        cap.request_context = None;
        let mut levels = ladder(&["low", "medium", "high"], P);
        levels.insert(ModelThinkingLevel::Xhigh, CursorLevelSpec { value: "extra-high".to_owned(), encoding: P });
        cap.levels = levels;
        cap
    });
    m.insert("gpt-5.1", {
        let mut cap = gpt(&[]);
        cap.parameter_order = vec!["reasoning"];
        cap.max_window = None;
        cap.default_context = None;
        cap.request_context = None;
        cap.levels = ladder(&["low", "high"], P);
        cap
    });
    m.insert("gpt-5.2", {
        let mut cap = gpt(&[]);
        cap.parameter_order = vec!["reasoning"];
        cap.max_window = None;
        cap.default_context = None;
        cap.request_context = None;
        let mut levels = ladder(&["low", "high"], P);
        levels.insert(ModelThinkingLevel::Xhigh, CursorLevelSpec { value: "xhigh".to_owned(), encoding: V });
        cap.levels = levels;
        cap
    });
    m.insert("gpt-5.4", {
        let mut cap = gpt(&[]);
        cap.parameter_order = vec!["reasoning"];
        cap.max_window = None;
        cap.default_context = None;
        cap.request_context = None;
        let mut levels = ladder(&["low", "medium", "high"], P);
        levels.insert(ModelThinkingLevel::Xhigh, CursorLevelSpec { value: "xhigh".to_owned(), encoding: V });
        cap.levels = levels;
        cap
    });
    m.insert(
        "gpt-5.4-mini",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 400_000.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: ladder(&["none", "low", "medium", "high", "xhigh"], V),
        },
    );
    m.insert("gpt-5.4-nano", {
        let mut cap = gpt(&[]);
        cap.parameter_order = vec!["reasoning"];
        cap.max_window = None;
        cap.default_context = None;
        cap.request_context = None;
        cap.levels = ladder(&["none", "low", "medium", "high", "xhigh"], P);
        cap
    });
    m.insert(
        "gemini-3.7-flash",
        CursorModelCapability {
            catalog_key: None,
            evidence: "cli-live",
            window: 1_048_576.0,
            max_window: None,
            parameter_order: vec!["effort"],
            default_context: None,
            request_context: None,
            levels: ladder(&["low", "medium", "high"], P),
        },
    );
    m.insert(
        "gemini-3.6-flash",
        CursorModelCapability {
            catalog_key: None,
            evidence: "suffix-only",
            window: 1_048_576.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: ladder(&["minimal", "low", "medium", "high"], V),
        },
    );
    m.insert(
        "cursor-grok-4.6",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 500_000.0,
            max_window: None,
            parameter_order: vec!["effort", "fast"],
            default_context: None,
            request_context: None,
            levels: ladder(&["low", "medium", "high", "xhigh"], P),
        },
    );
    m.insert(
        "cursor-grok-4.5",
        CursorModelCapability {
            catalog_key: None,
            evidence: "suffix-only",
            window: 500_000.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: ladder(&["low", "medium", "high"], V),
        },
    );
    m.insert(
        "glm-5.2",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 1_000_000.0,
            max_window: None,
            parameter_order: vec!["reasoning"],
            default_context: None,
            request_context: None,
            levels: ladder(&["high", "max"], P),
        },
    );
    m.insert(
        "kimi-k3",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 1_048_576.0,
            max_window: None,
            parameter_order: vec!["reasoning"],
            default_context: None,
            request_context: None,
            levels: ladder(&["low", "high", "max"], P),
        },
    );
    m.insert(
        "composer-2.5",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 200_000.0,
            max_window: None,
            parameter_order: vec!["fast"],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "claude-haiku-4-5",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 200_000.0,
            max_window: None,
            parameter_order: vec!["thinking"],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "claude-4-sonnet",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 200_000.0,
            max_window: None,
            parameter_order: vec!["thinking", "context"],
            default_context: Some("200k".to_owned()),
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "claude-4.5-sonnet",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 200_000.0,
            max_window: None,
            parameter_order: vec!["thinking", "context"],
            default_context: Some("200k".to_owned()),
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "kimi-k2.7-code",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 262_144.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "gemini-3-flash",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 1_000_000.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "gemini-3.1-pro",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 1_000_000.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "gemini-3.5-flash",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 1_048_576.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m.insert(
        "gpt-5-mini",
        CursorModelCapability {
            catalog_key: None,
            evidence: "available-models",
            window: 400_000.0,
            max_window: None,
            parameter_order: vec![],
            default_context: None,
            request_context: None,
            levels: BTreeMap::new(),
        },
    );
    m
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorVariantAlias {
    pub target_id: String,
    pub level: Option<ModelThinkingLevel>,
    pub legacy_variant_id: String,
    /// Encoding is always "legacy-variant" in the TS source.
    pub encoding: &'static str,
}

#[derive(serde::Deserialize)]
struct RawAliasEntry {
    #[serde(rename = "targetId")]
    target_id: String,
    #[serde(default)]
    level: Option<String>,
    #[serde(rename = "legacyVariantId")]
    legacy_variant_id: String,
}

#[derive(serde::Deserialize)]
struct RawAliasData {
    aliases: BTreeMap<String, RawAliasEntry>,
}

static ALIASES: LazyLock<BTreeMap<String, CursorVariantAlias>> = LazyLock::new(|| {
    let raw: RawAliasData =
        serde_json::from_str(include_str!("cursor-variant-aliases.json")).expect("cursor-variant-aliases.json");
    raw.aliases
        .into_iter()
        .map(|(id, entry)| {
            let level = entry.level.as_deref().map(level_token_to_thinking_level);
            (
                id,
                CursorVariantAlias {
                    target_id: entry.target_id,
                    level,
                    legacy_variant_id: entry.legacy_variant_id,
                    encoding: "legacy-variant",
                },
            )
        })
        .collect()
});

const LEVEL_TOKENS: &[&str] = &["minimal", "low", "medium", "high", "extra-high", "xhigh", "max", "none"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorVariantParse {
    pub base_id: String,
    pub level: Option<String>,
    pub thinking: Option<bool>,
    pub fast: bool,
    pub original_id: String,
}

/// Lossless suffix parser: trailing `-fast`, `-thinking-<level>` or `-<level>-thinking`, bare `-thinking`, trailing level.
pub fn parse_cursor_variant_id(original_id: &str) -> CursorVariantParse {
    let mut rest = original_id.to_owned();
    let mut fast = false;
    if let Some(stripped) = rest.strip_suffix("-fast") {
        fast = true;
        rest = stripped.to_owned();
    }
    let alternation = LEVEL_TOKENS.join("|");

    for thinking_first in [true, false] {
        let pattern = if thinking_first {
            format!(r"-thinking-({alternation})$")
        } else {
            format!(r"-({alternation})-thinking$")
        };
        let re = regex::Regex::new(&pattern).expect("valid regex");
        if let Some(captures) = re.captures(&rest) {
            let level = captures.get(1).map(|m| m.as_str().to_owned());
            let match_start = captures.get(0).expect("full match").start();
            let base_id = rest[..match_start].to_owned();
            return CursorVariantParse {
                base_id,
                level,
                thinking: Some(true),
                fast,
                original_id: original_id.to_owned(),
            };
        }
    }

    if let Some(stripped) = rest.strip_suffix("-thinking") {
        return CursorVariantParse {
            base_id: stripped.to_owned(),
            level: None,
            thinking: Some(true),
            fast,
            original_id: original_id.to_owned(),
        };
    }

    let level_re = regex::Regex::new(&format!(r"-({alternation})$")).expect("valid regex");
    let (base_id, level, thinking) = if let Some(captures) = level_re.captures(&rest) {
        let level = captures.get(1).map(|m| m.as_str().to_owned());
        let match_start = captures.get(0).expect("full match").start();
        (rest[..match_start].to_owned(), level, Some(false))
    } else {
        (rest.clone(), None, None)
    };

    CursorVariantParse { base_id, level, thinking, fast, original_id: original_id.to_owned() }
}

pub fn get_cursor_variant_alias(original_id: &str) -> Option<CursorVariantAlias> {
    ALIASES.get(original_id).cloned()
}

/// Every alias entry in the static table, for callers that need to scan target ids
/// (e.g. the `STATIC_TARGETS` set in `catalog-grouping.ts`).
pub fn all_cursor_variant_aliases() -> Vec<CursorVariantAlias> {
    ALIASES.values().cloned().collect()
}

/// Resolve any known variant id to its selectable base identity; unknown ids return `None`.
pub fn get_cursor_base_id_for_variant(original_id: &str) -> Option<String> {
    let alias = ALIASES.get(original_id)?;
    Some(parse_cursor_variant_id(&alias.target_id).base_id)
}

pub fn get_cursor_capability_for_base(base_id: &str) -> Option<CursorModelCapability> {
    CURSOR_MODEL_CAPABILITIES.get(base_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected_windows() -> Vec<(&'static str, f64)> {
        vec![
            ("kimi-k3", 1_048_576.0),
            ("glm-5.2", 1_000_000.0),
            ("gemini-3.6-flash", 1_048_576.0),
            ("gemini-3.7-flash", 1_048_576.0),
            ("gpt-5.1", 400_000.0),
            ("gpt-5.2", 400_000.0),
            ("gpt-5.3-codex", 400_000.0),
            ("gpt-5.4", 400_000.0),
            ("gpt-5.4-mini", 400_000.0),
            ("gpt-5.4-nano", 400_000.0),
            ("gpt-5.5", 1_000_000.0),
            ("gpt-5.6-sol", 1_000_000.0),
            ("gpt-5.6-luna", 1_000_000.0),
            ("gpt-5.6-terra", 272_000.0),
            ("cursor-grok-4.5", 500_000.0),
            ("cursor-grok-4.6", 500_000.0),
            ("kimi-k2.7-code", 262_144.0),
            ("claude-4.6-sonnet", 1_000_000.0),
            ("claude-4.6-opus", 1_000_000.0),
            ("claude-fable-5", 1_000_000.0),
            ("claude-sonnet-5", 1_000_000.0),
            ("claude-opus-4-7", 1_000_000.0),
            ("claude-opus-4-8", 1_000_000.0),
            ("claude-opus-5", 1_000_000.0),
            ("composer-2.5", 200_000.0),
            ("claude-haiku-4-5", 200_000.0),
            ("claude-4-sonnet", 200_000.0),
            ("claude-4.5-sonnet", 200_000.0),
            ("claude-4.5-opus", 200_000.0),
        ]
    }

    /// C1 — capability table and context windows.
    #[test]
    fn assigns_the_exact_live_catalog_window_to_every_family_base() {
        for (base, window) in expected_windows() {
            let cap = get_cursor_capability_for_base(base);
            assert!(cap.is_some(), "missing capability for {base}");
            assert_eq!(cap.expect("cap").window, window, "{base} window");
        }
    }

    #[test]
    fn keeps_window_and_max_window_distinct() {
        let fable = get_cursor_capability_for_base("claude-fable-5").expect("fable");
        assert_eq!(fable.window, 1_000_000.0);
        assert_eq!(fable.max_window, Some(1_000_000.0));
        assert_eq!(fable.default_context, Some("300k".to_owned()));
        let kimi = get_cursor_capability_for_base("kimi-k3").expect("kimi");
        assert_eq!(kimi.window, 1_048_576.0);
        assert_eq!(kimi.max_window, None);
    }

    #[test]
    fn resolves_a_variant_id_to_its_base_capability_with_the_same_window() {
        assert_eq!(get_cursor_base_id_for_variant("kimi-k3-max"), Some("kimi-k3".to_owned()));
        assert_eq!(
            get_cursor_base_id_for_variant("claude-fable-5-thinking-xhigh"),
            Some("claude-fable-5".to_owned())
        );
        assert_eq!(
            get_cursor_base_id_for_variant("claude-opus-4-7-thinking-high-fast"),
            Some("claude-opus-4-7".to_owned())
        );
        let base = get_cursor_base_id_for_variant("kimi-k3-max").expect("base");
        assert_eq!(get_cursor_capability_for_base(&base).expect("cap").window, 1_048_576.0);
    }

    #[test]
    fn covers_every_parsed_base_from_the_204_id_live_fixture_or_documents_the_fallback() {
        #[derive(serde::Deserialize)]
        struct FixtureEntry {
            id: String,
        }
        let fixture: Vec<FixtureEntry> = serde_json::from_str(include_str!(
            "../../tests/fixtures/cursor-usable-models-20260818.json"
        ))
        .expect("fixture");
        let mut uncovered: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for entry in &fixture {
            let parsed = parse_cursor_variant_id(&entry.id);
            if get_cursor_capability_for_base(&parsed.base_id).is_none() {
                uncovered.insert(parsed.base_id);
            }
        }
        let uncovered: Vec<String> = uncovered.into_iter().collect();
        assert_eq!(uncovered, vec!["default".to_owned()]);
    }

    #[test]
    fn marks_evidence_provenance_for_every_entry() {
        for (id, cap) in CURSOR_MODEL_CAPABILITIES.iter() {
            assert!(
                ["available-models", "cli-live", "suffix-only"].contains(&cap.evidence),
                "{id} evidence"
            );
        }
    }
}
