use indexmap::IndexMap;
use maho_ai::types::{Model, ModelThinkingLevel};

pub const MAX_PROVIDERS_PER_FAMILY: usize = 2;
const PRECEDENCE: &[&str] = &["anthropic-subscription", "anthropic", "kimi-coding"];

/// The only providers a bare selector reaches: each model vendor's own lanes, keyed by the vendor's
/// model-id prefix (senpi#5518).
pub const FIRST_PARTY_PROVIDERS: &[(&str, &[&str])] = &[
    ("claude-", &["anthropic-subscription", "anthropic"]),
    ("gpt-", &["chatgpt-subscription", "openai"]),
    ("gemini-", &["google", "google-vertex"]),
    ("kimi-", &["kimi-coding", "moonshotai", "moonshotai-cn"]),
    ("glm-", &["zai", "zai-coding-cn"]),
    ("grok-", &["xai"]),
    ("deepseek-", &["deepseek"]),
    ("minimax-", &["minimax", "minimax-cn"]),
    (
        "mimo-",
        &[
            "xiaomi",
            "xiaomi-token-plan-cn",
            "xiaomi-token-plan-sgp",
            "xiaomi-token-plan-ams",
        ],
    ),
    (
        "qwen",
        &[
            "qwen-token-plan",
            "qwen-token-plan-individual",
            "qwen-token-plan-cn",
            "alibaba-token-plan",
        ],
    ),
];

fn is_k_digit_family(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next() == Some(b'k') && bytes.next().is_some_and(|b| b.is_ascii_digit())
}

fn first_party_vendor_providers(family: &str) -> Option<&'static [&'static str]> {
    let f = family.to_ascii_lowercase();
    if f.starts_with("claude-") {
        return Some(&["anthropic-subscription", "anthropic"]);
    }
    if f.starts_with("gpt-")
        || (f.starts_with('o') && f.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
    {
        return Some(&["chatgpt-subscription", "openai"]);
    }
    if f.starts_with("gemini-") {
        return Some(&["google", "google-vertex"]);
    }
    if f.starts_with("kimi-") || is_k_digit_family(&f) {
        return Some(&["kimi-coding", "moonshotai", "moonshotai-cn"]);
    }
    if f.starts_with("glm-") {
        return Some(&["zai", "zai-coding-cn"]);
    }
    if f.starts_with("grok-") {
        return Some(&["xai"]);
    }
    if f.starts_with("deepseek-") {
        return Some(&["deepseek"]);
    }
    if [
        "mistral",
        "codestral",
        "devstral",
        "magistral",
        "ministral",
        "pixtral",
    ]
    .iter()
    .any(|prefix| f.starts_with(prefix))
    {
        return Some(&["mistral"]);
    }
    if f.starts_with("minimax-") {
        return Some(&["minimax", "minimax-cn"]);
    }
    if f.starts_with("mimo-") {
        return Some(&[
            "xiaomi",
            "xiaomi-token-plan-cn",
            "xiaomi-token-plan-sgp",
            "xiaomi-token-plan-ams",
        ]);
    }
    if f.starts_with("qwen") {
        return Some(&[
            "qwen-token-plan",
            "qwen-token-plan-individual",
            "qwen-token-plan-cn",
            "alibaba-token-plan",
        ]);
    }
    None
}

fn in_bare_scope(provider: &str, family: &str, session_provider: Option<&str>) -> bool {
    let normalized = provider.to_ascii_lowercase();
    let vendor = first_party_vendor_providers(family);
    let Some(session) = session_provider else {
        return vendor.is_none_or(|providers| providers.contains(&normalized.as_str()));
    };
    let session = session.to_ascii_lowercase();
    if normalized == session {
        return true;
    }
    let Some(providers) = vendor else {
        return false;
    };
    if providers.contains(&session.as_str()) {
        return false;
    }
    providers.contains(&normalized.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BareSelectorParts {
    pub family: String,
    pub thinking_level: Option<ModelThinkingLevel>,
}

pub struct FallbackAuthTiers<'a> {
    pub is_using_oauth: &'a dyn Fn(&Model) -> bool,
    pub has_configured_auth: Option<&'a dyn Fn(&Model) -> bool>,
    pub is_fallback_eligible: Option<&'a dyn Fn(&Model) -> bool>,
}

pub fn parse_bare_selector(raw: &str) -> Option<BareSelectorParts> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains(['/', '*']) {
        return None;
    }
    if let Some((prefix, suffix)) = raw.rsplit_once(':')
        && let Some(level) = ModelThinkingLevel::parse(&suffix.to_lowercase())
    {
        let family = prefix.trim().to_lowercase();
        return (!family.is_empty()).then_some(BareSelectorParts {
            family,
            thinking_level: Some(level),
        });
    }
    Some(BareSelectorParts {
        family: raw.to_lowercase(),
        thinking_level: None,
    })
}

fn without_namespace(id: &str) -> &str {
    id.rsplit(['.', '/']).next().unwrap_or(id)
}

fn family_ids(model: &Model) -> Vec<String> {
    let id = model.id.to_lowercase();
    let bare = without_namespace(&id).to_owned();
    let mut ids = vec![id.clone(), bare];
    if is_k_digit_family(&id) {
        ids.push(format!("kimi-{id}"));
    }
    ids
}

fn exactness(model: &Model, family: &str) -> u8 {
    let id = model.id.to_lowercase();
    if id == family || without_namespace(&id) == family {
        2
    } else if family_ids(model)
        .iter()
        .any(|candidate| candidate == family)
    {
        1
    } else {
        0
    }
}

pub fn matches_family(model: &Model, family: &str) -> bool {
    family_ids(model)
        .iter()
        .any(|id| id == family || id.starts_with(&format!("{family}-")))
}

pub fn rank_family_models<'a>(
    models: &'a [Model],
    family: &str,
    tiers: &FallbackAuthTiers<'_>,
    limit: Option<usize>,
) -> Vec<&'a Model> {
    rank_family_models_with_session(models, family, tiers, limit, None)
}

pub fn rank_family_models_with_session<'a>(
    models: &'a [Model],
    family: &str,
    tiers: &FallbackAuthTiers<'_>,
    limit: Option<usize>,
    session_provider: Option<&str>,
) -> Vec<&'a Model> {
    let mut grouped: IndexMap<&str, Vec<&Model>> = IndexMap::new();
    for model in models {
        if !in_bare_scope(&model.provider, family, session_provider)
            || !matches_family(model, family)
            || tiers.is_fallback_eligible.is_some_and(|f| !f(model))
        {
            continue;
        }
        grouped.entry(&model.provider).or_default().push(model);
    }
    // The session's own provider serves the family: the rung stays there and never reaches another lane.
    let session_lower = session_provider.map(str::to_ascii_lowercase);
    let own = session_lower.as_deref().and_then(|s| {
        grouped
            .iter()
            .find(|(provider, _)| provider.to_ascii_lowercase() == s)
            .map(|(_, variants)| variants)
    });
    let mut picked: Vec<&Model> = match own {
        Some(variants) => {
            let mut sorted = variants.clone();
            sorted.sort_by(|a, b| {
                exactness(b, family)
                    .cmp(&exactness(a, family))
                    .then_with(|| a.id.len().cmp(&b.id.len()))
                    .then_with(|| a.id.cmp(&b.id))
            });
            sorted.first().copied().into_iter().collect()
        }
        None => grouped
            .values_mut()
            .filter_map(|variants| {
                variants.sort_by(|a, b| {
                    exactness(b, family)
                        .cmp(&exactness(a, family))
                        .then_with(|| a.id.len().cmp(&b.id.len()))
                        .then_with(|| a.id.cmp(&b.id))
                });
                variants.first().copied()
            })
            .collect(),
    };
    let tier = |m: &Model| {
        if (tiers.is_using_oauth)(m) {
            0
        } else if tiers.has_configured_auth.is_some_and(|f| f(m)) {
            1
        } else {
            2
        }
    };
    let precedence = |m: &Model| {
        PRECEDENCE
            .iter()
            .position(|p| *p == m.provider.to_lowercase())
            .unwrap_or(PRECEDENCE.len())
    };
    picked.sort_by(|a, b| {
        tier(a)
            .cmp(&tier(b))
            .then_with(|| precedence(a).cmp(&precedence(b)))
            .then_with(|| a.provider.cmp(&b.provider))
    });
    if let Some(limit) = limit {
        picked.truncate(limit);
    }
    picked
}
