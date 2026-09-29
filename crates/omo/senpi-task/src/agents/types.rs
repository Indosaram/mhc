//! Agent definition shapes (`agents/types.ts`, `agents/agent-model-entry.ts`).

use omo_config_core::OmoConfigEnv;

use crate::model_chain::ModelChainCandidate;

/// One last-match-wins tool rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolRule {
    pub pattern: String,
    pub allow: bool,
}

impl AgentToolRule {
    pub fn new(pattern: &str, allow: bool) -> Self {
        Self {
            pattern: pattern.to_string(),
            allow,
        }
    }
}

/// The object spelling of a `models[]` entry: a fallback model with its own tuning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentModelCandidate {
    pub model: String,
    pub variant: Option<String>,
    /// Canonical reasoning spelling; wins over `reasoning_effort` on the same entry.
    pub reasoning: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// A `models[]` entry: a bare model string or an object carrying per-entry tuning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentModelEntry {
    Model(String),
    Candidate(AgentModelCandidate),
}

impl AgentModelEntry {
    pub fn model(&self) -> &str {
        match self {
            Self::Model(model) => model,
            Self::Candidate(candidate) => &candidate.model,
        }
    }
}

/// A task agent definition. `None` fields are absent (the TS spread omits `undefined` keys).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentDefinition {
    pub name: String,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub models: Option<Vec<AgentModelEntry>>,
    pub variant: Option<String>,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub tools: Option<Vec<AgentToolRule>>,
    pub disable: Option<bool>,
    pub background: Option<bool>,
    pub execution_mode: Option<String>,
    pub allowed_subagents: Option<Vec<String>>,
    pub disallowed_tools: Option<Vec<String>>,
    pub max_depth: Option<u64>,
    pub max_turns: Option<u64>,
}

impl AgentDefinition {
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }

    /// `{ ...self, ...overlay }`: every field the overlay defines wins.
    pub(crate) fn overlaid_with(self, overlay: Self) -> Self {
        Self {
            name: overlay.name,
            description: overlay.description.or(self.description),
            prompt: overlay.prompt.or(self.prompt),
            mode: overlay.mode.or(self.mode),
            model: overlay.model.or(self.model),
            models: overlay.models.or(self.models),
            variant: overlay.variant.or(self.variant),
            reasoning_effort: overlay.reasoning_effort.or(self.reasoning_effort),
            temperature: overlay.temperature.or(self.temperature),
            tools: overlay.tools.or(self.tools),
            disable: overlay.disable.or(self.disable),
            background: overlay.background.or(self.background),
            execution_mode: overlay.execution_mode.or(self.execution_mode),
            allowed_subagents: overlay.allowed_subagents.or(self.allowed_subagents),
            disallowed_tools: overlay.disallowed_tools.or(self.disallowed_tools),
            max_depth: overlay.max_depth.or(self.max_depth),
            max_turns: overlay.max_turns.or(self.max_turns),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLoaderDiagnosticKind {
    Frontmatter,
    Read,
    Validation,
    ConfigParse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoaderDiagnostic {
    pub kind: AgentLoaderDiagnosticKind,
    pub path: String,
    pub message: String,
    pub issue_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct LoadAgentsOptions {
    pub env: Option<OmoConfigEnv>,
    pub home_dir: Option<String>,
    pub project_dir: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LoadAgentsResult {
    /// Insertion-ordered like the TS `Object.fromEntries(Map)`.
    pub agents: Vec<(String, AgentDefinition)>,
    pub diagnostics: Vec<AgentLoaderDiagnostic>,
}

impl LoadAgentsResult {
    pub fn agent(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents
            .iter()
            .find(|(agent_name, _)| agent_name == name)
            .map(|(_, definition)| definition)
    }
}

/// Primary model first, then `models[]`; per-entry tuning wins over the agent-level default so a
/// fallback entry can run at a different effort than the primary model.
pub fn agent_model_candidates(
    primary: Option<&str>,
    entries: Option<&[AgentModelEntry]>,
    default_variant: Option<&str>,
    default_reasoning_effort: Option<&str>,
) -> Vec<ModelChainCandidate> {
    let with_defaults = |candidate: AgentModelCandidate| ModelChainCandidate {
        model: candidate.model,
        variant: candidate
            .variant
            .or_else(|| default_variant.map(str::to_string)),
        reasoning_effort: candidate
            .reasoning
            .or(candidate.reasoning_effort)
            .or_else(|| default_reasoning_effort.map(str::to_string)),
    };
    let primary = primary.map(|model| AgentModelCandidate {
        model: model.to_string(),
        ..AgentModelCandidate::default()
    });
    let chain = entries.unwrap_or_default().iter().map(|entry| match entry {
        AgentModelEntry::Model(model) => AgentModelCandidate {
            model: model.clone(),
            ..AgentModelCandidate::default()
        },
        AgentModelEntry::Candidate(candidate) => candidate.clone(),
    });
    primary
        .into_iter()
        .chain(chain)
        .map(with_defaults)
        .collect()
}
