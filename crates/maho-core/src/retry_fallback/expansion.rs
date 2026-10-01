use indexmap::IndexMap;
use maho_ai::types::{Model, ModelThinkingLevel};

pub const MAX_PROVIDERS_PER_FAMILY: usize = 2;
const DENYLIST: &[&str] = &["cursor", "openrouter", "openrouter-images"];
const PRECEDENCE: &[&str] = &["anthropic-subscription", "anthropic", "kimi-coding"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BareSelectorParts { pub family: String, pub thinking_level: Option<ModelThinkingLevel> }

pub struct FallbackAuthTiers<'a> {
    pub is_using_oauth: &'a dyn Fn(&Model) -> bool,
    pub has_configured_auth: Option<&'a dyn Fn(&Model) -> bool>,
    pub is_fallback_eligible: Option<&'a dyn Fn(&Model) -> bool>,
}

pub fn parse_bare_selector(raw: &str) -> Option<BareSelectorParts> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains(['/', '*']) { return None; }
    if let Some((prefix, suffix)) = raw.rsplit_once(':') && let Some(level) = ModelThinkingLevel::parse(&suffix.to_lowercase()) {
        let family = prefix.trim().to_lowercase();
        return (!family.is_empty()).then_some(BareSelectorParts { family, thinking_level: Some(level) });
    }
    Some(BareSelectorParts { family: raw.to_lowercase(), thinking_level: None })
}

fn without_namespace(id: &str) -> &str { id.rsplit(['.', '/']).next().unwrap_or(id) }

pub fn matches_family(model: &Model, family: &str) -> bool {
    let id = model.id.to_lowercase();
    [id.as_str(), without_namespace(&id)].iter().any(|id| *id == family || id.starts_with(&format!("{family}-")))
}

pub fn rank_family_models<'a>(models: &'a [Model], family: &str, tiers: &FallbackAuthTiers<'_>, limit: Option<usize>) -> Vec<&'a Model> {
    let mut grouped: IndexMap<&str, Vec<&Model>> = IndexMap::new();
    for model in models {
        if DENYLIST.contains(&model.provider.to_lowercase().as_str()) || tiers.is_fallback_eligible.is_some_and(|f| !f(model)) || !matches_family(model, family) { continue; }
        grouped.entry(&model.provider).or_default().push(model);
    }
    let mut picked: Vec<_> = grouped.values_mut().filter_map(|variants| {
        variants.sort_by(|a, b| {
            let ae = without_namespace(&a.id.to_lowercase()) == family;
            let be = without_namespace(&b.id.to_lowercase()) == family;
            be.cmp(&ae).then_with(|| a.id.len().cmp(&b.id.len())).then_with(|| a.id.cmp(&b.id))
        });
        variants.first().copied()
    }).collect();
    let tier = |m: &Model| if (tiers.is_using_oauth)(m) { 0 } else if tiers.has_configured_auth.is_some_and(|f| f(m)) { 1 } else { 2 };
    let precedence = |m: &Model| PRECEDENCE.iter().position(|p| *p == m.provider.to_lowercase()).unwrap_or(PRECEDENCE.len());
    picked.sort_by(|a, b| tier(a).cmp(&tier(b)).then_with(|| precedence(a).cmp(&precedence(b))).then_with(|| a.provider.cmp(&b.provider)));
    if let Some(limit) = limit { picked.truncate(limit); }
    picked
}
