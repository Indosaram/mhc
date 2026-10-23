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

pub(crate) fn level_token_to_thinking_level(value: &str) -> ModelThinkingLevel {
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

pub use crate::cursor::capability_table::CURSOR_MODEL_CAPABILITIES;

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
    let raw: RawAliasData = serde_json::from_str(include_str!("cursor-variant-aliases.json"))
        .expect("cursor-variant-aliases.json");
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

const LEVEL_TOKENS: &[&str] = &[
    "minimal",
    "low",
    "medium",
    "high",
    "extra-high",
    "xhigh",
    "max",
    "none",
];

static THINKING_LEVEL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(r"-thinking-({})$", LEVEL_TOKENS.join("|"))).expect("valid regex")
});
static LEVEL_THINKING_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(r"-({})-thinking$", LEVEL_TOKENS.join("|"))).expect("valid regex")
});
static LEVEL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(r"-({})$", LEVEL_TOKENS.join("|"))).expect("valid regex")
});

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
    for re in [&*THINKING_LEVEL_RE, &*LEVEL_THINKING_RE] {
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

    let (base_id, level, thinking) = if let Some(captures) = LEVEL_RE.captures(&rest) {
        let level = captures.get(1).map(|m| m.as_str().to_owned());
        let match_start = captures.get(0).expect("full match").start();
        (rest[..match_start].to_owned(), level, Some(false))
    } else {
        (rest.clone(), None, None)
    };

    CursorVariantParse {
        base_id,
        level,
        thinking,
        fast,
        original_id: original_id.to_owned(),
    }
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
        assert_eq!(
            get_cursor_base_id_for_variant("kimi-k3-max"),
            Some("kimi-k3".to_owned())
        );
        assert_eq!(
            get_cursor_base_id_for_variant("claude-fable-5-thinking-xhigh"),
            Some("claude-fable-5".to_owned())
        );
        assert_eq!(
            get_cursor_base_id_for_variant("claude-opus-4-7-thinking-high-fast"),
            Some("claude-opus-4-7".to_owned())
        );
        let base = get_cursor_base_id_for_variant("kimi-k3-max").expect("base");
        assert_eq!(
            get_cursor_capability_for_base(&base).expect("cap").window,
            1_048_576.0
        );
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
        // `default` (Auto) gained its server-reported window in senpi#3620.
        assert_eq!(uncovered, Vec::<String>::new());
    }

    #[test]
    fn marks_evidence_provenance_for_every_entry() {
        for (id, cap) in CURSOR_MODEL_CAPABILITIES.iter() {
            assert!(
                ["available-models", "cli-live", "suffix-only", "served-checkpoint"].contains(&cap.evidence),
                "{id} evidence"
            );
        }
    }

    /// models.dev first-party windows plus the context options Cursor offers each family.
    #[derive(serde::Deserialize)]
    struct FirstPartyWindow {
        context: f64,
    }

    #[derive(serde::Deserialize)]
    struct WindowSsot {
        #[serde(rename = "cursorContextEnum")]
        cursor_context_enum: BTreeMap<String, f64>,
        #[serde(rename = "cursorContextOptionsByFamily")]
        cursor_context_options_by_family: BTreeMap<String, Vec<String>>,
        #[serde(rename = "firstParty")]
        first_party: BTreeMap<String, FirstPartyWindow>,
        #[serde(rename = "cursorReportedWindow")]
        cursor_reported_window: BTreeMap<String, f64>,
    }

    fn window_ssot() -> WindowSsot {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/models-dev-first-party-windows-20260818.json"
        ))
        .expect("window ssot fixture")
    }

    /// Cursor only honours a window it actually offers that family, so the spec value is capped by it.
    fn expected_window(ssot: &WindowSsot, family: &str) -> Option<f64> {
        let options = ssot.cursor_context_options_by_family.get(family);
        if let Some(options) = options.filter(|options| !options.is_empty()) {
            let cap = options
                .iter()
                .filter_map(|option| ssot.cursor_context_enum.get(option))
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            let spec = ssot.first_party.get(family).map(|entry| entry.context);
            return Some(spec.map_or(cap, |spec| spec.min(cap)));
        }
        if let Some(spec) = ssot.first_party.get(family) {
            return Some(spec.context);
        }
        ssot.cursor_reported_window.get(family).copied()
    }

    #[test]
    fn caps_each_family_at_the_largest_context_option_cursor_offers_it() {
        let ssot = window_ssot();
        let mut drift: Vec<String> = Vec::new();
        for (family, capability) in CURSOR_MODEL_CAPABILITIES.iter() {
            let Some(expected) = expected_window(&ssot, family) else {
                continue;
            };
            if capability.window != expected {
                drift.push(format!(
                    "{family}: committed {} != SSOT {expected}",
                    capability.window
                ));
            }
        }
        assert_eq!(drift, Vec::<String>::new());
    }

    #[test]
    fn never_advertises_a_window_above_the_largest_context_option_cursor_offers() {
        let ssot = window_ssot();
        for (family, capability) in CURSOR_MODEL_CAPABILITIES.iter() {
            let Some(options) = ssot.cursor_context_options_by_family.get(*family) else {
                continue;
            };
            if options.is_empty() {
                continue;
            }
            let cap = options
                .iter()
                .filter_map(|option| ssot.cursor_context_enum.get(option))
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(
                capability.window <= cap,
                "{family} exceeds the context cursor offers it"
            );
        }
    }

    #[test]
    fn keeps_families_outside_the_ssot_on_a_positive_fallback_window() {
        let ssot = window_ssot();
        for (family, capability) in CURSOR_MODEL_CAPABILITIES.iter() {
            if expected_window(&ssot, family).is_some() {
                continue;
            }
            assert!(
                capability.window > 0.0,
                "{family} must carry a positive window"
            );
        }
    }

    /// Cursor lists these through its CLI only; an unproven capability window must not appear.
    const CLI_ONLY: &[&str] = &[
        "claude-mythos-5",
        "claude-sonnet-4-7",
        "composer-2.6",
        "composer-2.6-lite",
        "deepseek-v4",
        "gemini-3.5-pro",
        "gemini-3.6-pro",
        "gemini-3.7-pro",
        "gpt-5.4-codex",
    ];

    #[test]
    fn leaves_cli_only_families_without_a_capability_entry_so_they_take_the_documented_fallback() {
        for family in CLI_ONLY {
            assert!(
                !CURSOR_MODEL_CAPABILITIES.contains_key(family),
                "{family} is CLI-only and must not gain an unproven capability window"
            );
        }
    }

    #[test]
    fn keeps_genuinely_small_window_families_off_the_1m_promotion() {
        for family in ["claude-4.5-opus", "claude-haiku-4-5", "composer-2.5"] {
            let capability = CURSOR_MODEL_CAPABILITIES
                .get(family)
                .unwrap_or_else(|| panic!("missing capability for {family}"));
            assert_eq!(
                capability.window, 200_000.0,
                "{family} must stay at its first-party 200000"
            );
        }
    }

    #[test]
    fn never_lets_a_family_advertise_more_than_its_own_max_window_when_one_is_declared() {
        for (family, capability) in CURSOR_MODEL_CAPABILITIES.iter() {
            let Some(max_window) = capability.max_window else {
                continue;
            };
            assert!(
                capability.window <= max_window,
                "{family} window exceeds maxWindow"
            );
        }
    }
}
