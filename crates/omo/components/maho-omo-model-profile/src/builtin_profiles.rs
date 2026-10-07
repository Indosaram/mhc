//! Port of omo-senpi `components/model-profile/builtin-profiles.ts` at omo `455dee62`: the named
//! model-chain table a human picks by lane.
//!
//! A model profile is a named, ordered model chain a human picks by lane ("Daily · Normal",
//! "Geeky · Heavy") instead of by model id. It is not the `profiles` key in omo.json: that one is a
//! VSCode-style config-layer overlay activated by `OMO_PROFILE`.
//!
//! Every rung is `{ providers, model, variant? }` - the exact shape the category fallback chains
//! use - and NOT a single `provider/model` string, so a Copilot-only, Bedrock-only or gateway-only
//! user still resolves the model instead of reading "unavailable" while the model sits right there
//! in the registry. Provider spellings are copied from those chains: senpi-only `kimi-coding` plus
//! the leftover OpenCode `kimi-for-coding` alias those chains keep, and the engine GLM ids `zai` /
//! `zai-coding-cn` (not OpenCode's `zai-coding-plan`; #8824).
//!
//! The table is additive data: an `omo.json` `model_profiles.<name>` entry replaces the builtin of
//! the same name WHOLESALE (see [`crate::resolve`]), so a later change here can never silently
//! override a chain a user wrote.
//!
//! There is no alias, migration, or compatibility shim for retired ids (`capable`, `deep-work`,
//! `simple-work`). A stale `model_profile` value is `unknown`.
//!
//! Builtin GPT rungs rank providers exactly like the `deep-low` / `deep-high` category chains
//! (#8737): the ChatGPT subscription first, then the `openai` API/proxy lane, then Copilot and
//! OpenCode where they serve the model. A user overlay that names `openai/` is scoped to that
//! provider.
//!
//! Unset sessions run `recommended`, which is not a lane (no family/tier): the same ladder senpi's
//! `recommended-models` builtin ships (`RECOMMENDED_DEFAULT_MODELS`, senpi#2074, with GPT-6.1 Sol in
//! the GPT-6 Sol slot from senpi#2394), so the TUI and the desktop start from one order. OmO carries
//! one extra rung: `gpt-6-sol` (medium) right behind `gpt-6.1-sol`, because 6.1 Sol is served only on
//! the two [OI] lanes and a Copilot or OpenCode Zen user must still reach a GPT-6 Sol rung. Every
//! builtin rung, in `recommended` and in the lanes, is served ONLY by its listed providers: a
//! gateway aggregator's vendor-prefixed copy (`opengateway/anthropic/`) never becomes the session
//! model (#9146).

use indexmap::IndexMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileFamily {
    Daily,
    Geeky,
}

impl ModelProfileFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Geeky => "geeky",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileTier {
    Normal,
    Heavy,
}

impl ModelProfileTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Heavy => "heavy",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinModelProfile {
    pub display_name: &'static str,
    pub description: &'static str,
    /// Picker axes; absent on `recommended`, which is the default rather than a lane.
    pub family: Option<ModelProfileFamily>,
    pub tier: Option<ModelProfileTier>,
    pub models: &'static [BuiltinRung],
}

/// A builtin rung. `providers` and `model` are `&'static str` so the table is `const`-friendly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinRung {
    pub providers: &'static [&'static str],
    pub model: &'static str,
    pub variant: Option<&'static str>,
}

/// Fresh-session default when `model_profile` is unset. Not written back to config.
pub const DEFAULT_MODEL_PROFILE_ID: &str = "recommended";

const CLAUDE_PROVIDERS: &[&str] = &["anthropic-subscription", "anthropic", "anthropic-api", "github-copilot", "opencode"];
const KIMI_PROVIDERS: &[&str] = &["kimi-coding", "kimi-for-coding", "moonshotai", "opencode-go"];
// Engine Z.AI ids. `omo setup` imports OpenCode's `zai-coding-plan` key as `zai` (#8799).
const GLM_PROVIDERS: &[&str] = &["zai", "zai-coding-cn", "opencode-go"];
const GPT_PROVIDERS: &[&str] = &["chatgpt-subscription", "openai", "github-copilot", "opencode"];
// GPT-6.1 Sol and its Fast tier are served only on the two [OI] lanes (not Copilot or OpenCode Zen),
// so their rungs list just those; the rung behind them (GPT-5.6 Sol in Geeky · Normal, GPT-6 Sol in
// Recommended) keeps the profile on every GPT provider.
const GPT_6_1_PROVIDERS: &[&str] = &["chatgpt-subscription", "openai"];

const RECOMMENDED_MODELS: &[BuiltinRung] = &[
    BuiltinRung { providers: CLAUDE_PROVIDERS, model: "claude-opus-5-5", variant: Some("medium") },
    BuiltinRung { providers: CLAUDE_PROVIDERS, model: "claude-fable-5-1", variant: Some("xhigh") },
    BuiltinRung { providers: KIMI_PROVIDERS, model: "kimi-k3", variant: Some("max") },
    BuiltinRung { providers: GPT_PROVIDERS, model: "gpt-6-astra", variant: Some("xhigh") },
    BuiltinRung { providers: GPT_6_1_PROVIDERS, model: "gpt-6.1-sol", variant: Some("medium") },
    BuiltinRung { providers: GPT_PROVIDERS, model: "gpt-6-sol", variant: Some("medium") },
    BuiltinRung { providers: GLM_PROVIDERS, model: "glm-5.3", variant: Some("max") },
];
const DAILY_NORMAL_MODELS: &[BuiltinRung] = &[
    BuiltinRung { providers: CLAUDE_PROVIDERS, model: "claude-opus-5-5", variant: Some("medium") },
    BuiltinRung { providers: KIMI_PROVIDERS, model: "kimi-k3", variant: Some("max") },
    BuiltinRung { providers: GLM_PROVIDERS, model: "glm-5.3", variant: Some("max") },
];
const DAILY_HEAVY_MODELS: &[BuiltinRung] =
    &[BuiltinRung { providers: CLAUDE_PROVIDERS, model: "claude-fable-5-1", variant: Some("xhigh") }];
const GEEKY_NORMAL_MODELS: &[BuiltinRung] = &[
    BuiltinRung { providers: GPT_6_1_PROVIDERS, model: "gpt-6.1-sol-fast", variant: Some("medium") },
    BuiltinRung { providers: GPT_6_1_PROVIDERS, model: "gpt-6.1-sol", variant: Some("medium") },
    BuiltinRung { providers: GPT_PROVIDERS, model: "gpt-5.6-sol", variant: Some("medium") },
];
const GEEKY_HEAVY_MODELS: &[BuiltinRung] =
    &[BuiltinRung { providers: GPT_PROVIDERS, model: "gpt-6-astra", variant: Some("high") }];

// Key order is the order a picker renders. `deep` is deliberately NOT an id: builtin delegation
// categories already carry that name, and the two axes never compete (a profile picks the MAIN
// session model; categories keep their own chains).
//
// Every Claude rung is headed by `anthropic-subscription`, senpi's Claude subscription lane,
// exactly like the category chains (#8051): rung provider order IS the ranking.
pub fn builtin_model_profiles() -> IndexMap<&'static str, BuiltinModelProfile> {
    let mut profiles = IndexMap::new();
    profiles.insert(
        "recommended",
        BuiltinModelProfile {
            display_name: "Recommended",
            description: "The best model you have connected, in OmO's recommended order.",
            family: None,
            tier: None,
            models: RECOMMENDED_MODELS,
        },
    );
    profiles.insert(
        "daily-normal",
        BuiltinModelProfile {
            display_name: "Daily · Normal",
            description: "Gets any task done without fuss.",
            family: Some(ModelProfileFamily::Daily),
            tier: Some(ModelProfileTier::Normal),
            models: DAILY_NORMAL_MODELS,
        },
    );
    profiles.insert(
        "daily-heavy",
        BuiltinModelProfile {
            display_name: "Daily · Heavy",
            description: "Gets any task done, after thinking it over from more sides.",
            family: Some(ModelProfileFamily::Daily),
            tier: Some(ModelProfileTier::Heavy),
            models: DAILY_HEAVY_MODELS,
        },
    );
    profiles.insert(
        "geeky-normal",
        BuiltinModelProfile {
            display_name: "Geeky · Normal",
            description: "Works on one task and thinks it through.",
            family: Some(ModelProfileFamily::Geeky),
            tier: Some(ModelProfileTier::Normal),
            models: GEEKY_NORMAL_MODELS,
        },
    );
    profiles.insert(
        "geeky-heavy",
        BuiltinModelProfile {
            display_name: "Geeky · Heavy",
            description: "Works on one task and thinks it over from every side.",
            family: Some(ModelProfileFamily::Geeky),
            tier: Some(ModelProfileTier::Heavy),
            models: GEEKY_HEAVY_MODELS,
        },
    );
    profiles
}
