//! Port of senpi packages/ai/src/cursor/capability-table.ts.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::cursor::model_capabilities::{
    CursorLevelEncoding, CursorLevelSpec, CursorModelCapability, CursorParameterId,
    level_token_to_thinking_level,
};
use crate::types::ModelThinkingLevel;

const P: CursorLevelEncoding = "parameters";
const V: CursorLevelEncoding = "variant-id";

fn ladder(
    values: &[&str],
    encoding: CursorLevelEncoding,
) -> BTreeMap<ModelThinkingLevel, CursorLevelSpec> {
    let mut out = BTreeMap::new();
    for value in values {
        out.insert(
            level_token_to_thinking_level(value),
            CursorLevelSpec {
                value: (*value).to_owned(),
                encoding,
            },
        );
    }
    out
}

const CLAUDE_ORDER: &[CursorParameterId] = &["thinking", "context", "effort"];
const GPT_ORDER: &[CursorParameterId] = &["context", "reasoning", "fast"];

fn claude(
    window: f64,
    max_window: f64,
    levels: &[&str],
    default_context: &str,
) -> CursorModelCapability {
    CursorModelCapability {
        catalog_key: None,
        evidence: "available-models",
        window,
        max_window: Some(max_window),
        parameter_order: CLAUDE_ORDER.to_vec(),
        default_context: Some(default_context.to_owned()),
        request_context: Some(if window >= 1_000_000.0 {
            "1m".to_owned()
        } else {
            default_context.to_owned()
        }),
        levels: ladder(levels, P),
    }
}

fn gpt_full(
    levels: &[&str],
    order: &[CursorParameterId],
    window: f64,
    max_window: f64,
) -> CursorModelCapability {
    let default_context = "272k".to_owned();
    let request_context = if order.contains(&"context") && window >= 1_000_000.0 {
        Some("1m".to_owned())
    } else {
        None
    };
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

const LEVELS_LOW_MAX: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const LEVELS_MINIMAL_MAX: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

fn context_token(window: f64) -> Option<&'static str> {
    if window == 300_000.0 {
        Some("300k")
    } else if window == 256_000.0 {
        Some("256k")
    } else {
        None
    }
}

/// A family whose window is the default context option the server reported serving.
fn served(
    window: f64,
    max_window: Option<f64>,
    order: &[CursorParameterId],
    levels: &[&str],
) -> CursorModelCapability {
    let context = if order.contains(&"context") {
        context_token(window).map(str::to_owned)
    } else {
        None
    };
    CursorModelCapability {
        catalog_key: None,
        evidence: "served-checkpoint",
        window,
        max_window,
        parameter_order: order.to_vec(),
        default_context: context.clone(),
        request_context: context,
        levels: ladder(levels, P),
    }
}

/// Static cursor capability table. Values come from the live
/// aiserver.v1 AvailableModels capture of 2026-08-18 (evidence:
/// local-ignore/qa-evidence/20260818-cursor-reasoning-levels/available-models-catalog.json).
/// `GetUsableModels` carries no window or parameter field, so this table is the
/// authoritative source; see .omo/plans/cursor-reasoning-levels.md §3.1/§6.
pub static CURSOR_MODEL_CAPABILITIES: LazyLock<BTreeMap<&'static str, CursorModelCapability>> =
    LazyLock::new(|| {
        let mut m = BTreeMap::new();
        m.insert(
            "claude-fable-5",
            claude(
                1_000_000.0,
                1_000_000.0,
                &["low", "medium", "high", "xhigh", "max"],
                "300k",
            ),
        );
        m.insert(
            "claude-sonnet-5",
            claude(
                1_000_000.0,
                1_000_000.0,
                &["low", "medium", "high", "xhigh", "max"],
                "300k",
            ),
        );
        m.insert(
            "claude-opus-4-7",
            claude(
                1_000_000.0,
                1_000_000.0,
                &["low", "medium", "high", "xhigh", "max"],
                "300k",
            ),
        );
        m.insert(
            "claude-opus-4-8",
            claude(
                1_000_000.0,
                1_000_000.0,
                &["low", "medium", "high", "xhigh", "max"],
                "300k",
            ),
        );
        m.insert(
            "claude-opus-5",
            claude(
                1_000_000.0,
                1_000_000.0,
                &["low", "medium", "high", "xhigh", "max"],
                "300k",
            ),
        );
        m.insert(
            "claude-4.6-opus",
            claude(1_000_000.0, 1_000_000.0, &["high", "max"], "200k"),
        );
        m.insert(
            "claude-4.6-sonnet",
            claude(1_000_000.0, 1_000_000.0, &["medium"], "200k"),
        );
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
                    CursorLevelSpec {
                        value: "high".to_owned(),
                        encoding: V,
                    },
                )]),
            },
        );
        m.insert(
            "gpt-5.6-sol",
            gpt_full(
                &["none", "low", "medium", "high", "xhigh", "max"],
                GPT_ORDER,
                1_000_000.0,
                1_000_000.0,
            ),
        );
        m.insert(
            "gpt-5.6-luna",
            gpt_full(
                &["none", "low", "medium", "high", "xhigh", "max"],
                GPT_ORDER,
                1_000_000.0,
                1_000_000.0,
            ),
        );
        m.insert(
            "gpt-5.6-terra",
            gpt_full(
                &["none", "low", "medium", "high", "xhigh", "max"],
                GPT_ORDER,
                272_000.0,
                272_000.0,
            ),
        );
        m.insert("gpt-5.5", {
            let mut cap = gpt_full(
                &["none", "low", "medium", "high"],
                GPT_ORDER,
                1_000_000.0,
                1_000_000.0,
            );
            let mut levels = ladder(&["none", "low", "medium", "high"], P);
            levels.insert(
                ModelThinkingLevel::Xhigh,
                CursorLevelSpec {
                    value: "extra-high".to_owned(),
                    encoding: P,
                },
            );
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
            levels.insert(
                ModelThinkingLevel::Xhigh,
                CursorLevelSpec {
                    value: "extra-high".to_owned(),
                    encoding: P,
                },
            );
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
            levels.insert(
                ModelThinkingLevel::Xhigh,
                CursorLevelSpec {
                    value: "xhigh".to_owned(),
                    encoding: V,
                },
            );
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
            levels.insert(
                ModelThinkingLevel::Xhigh,
                CursorLevelSpec {
                    value: "xhigh".to_owned(),
                    encoding: V,
                },
            );
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
        // Families published after the 2026-08-18 capture (senpi#3620): windows are the ceilings the
        // server reported on the first checkpoint on 2026-10-18.
        m.insert(
            "claude-opus-5-5",
            served(
                300_000.0,
                Some(1_000_000.0),
                &["context", "effort", "fast"],
                LEVELS_LOW_MAX,
            ),
        );
        m.insert(
            "claude-fable-5-1",
            served(
                300_000.0,
                Some(1_000_000.0),
                &["thinking", "context", "effort"],
                LEVELS_LOW_MAX,
            ),
        );
        m.insert(
            "claude-sonnet-5-5",
            served(
                300_000.0,
                Some(1_000_000.0),
                &["context", "reasoning_effort"],
                LEVELS_LOW_MAX,
            ),
        );
        m.insert(
            "claude-haiku-5-5",
            served(
                300_000.0,
                Some(1_000_000.0),
                &["thinking", "context", "reasoning_effort"],
                LEVELS_LOW_MAX,
            ),
        );
        m.insert(
            "muse-wonder-1.0",
            served(
                300_000.0,
                Some(1_000_000.0),
                &["context", "reasoning_effort"],
                LEVELS_MINIMAL_MAX,
            ),
        );
        m.insert(
            "muse-spark-1.3",
            served(
                300_000.0,
                Some(1_000_000.0),
                &["context", "effort"],
                LEVELS_MINIMAL_MAX,
            ),
        );
        m.insert(
            "grok-4.7",
            served(
                256_000.0,
                Some(500_000.0),
                &["context", "reasoning_effort", "fast"],
                &["low", "medium", "high", "xhigh"],
            ),
        );
        m.insert(
            "gemini-4-argon",
            served(
                200_000.0,
                None,
                &["reasoning_effort"],
                &["low", "medium", "high"],
            ),
        );
        m.insert(
            "gemini-3.8-flash",
            served(
                200_000.0,
                None,
                &["reasoning_effort"],
                &["low", "medium", "high"],
            ),
        );
        m.insert("default", served(256_000.0, None, &[], &[]));
        m
    });
