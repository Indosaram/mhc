//! Execution mode precedence (`manager/execution-mode.ts`).

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ExecutionMode {
    #[default]
    InProcess,
    Process,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in-process",
            Self::Process => "process",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "in-process" => Some(Self::InProcess),
            "process" => Some(Self::Process),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionModeSources {
    pub spec_mode: Option<ExecutionMode>,
    pub agent_mode: Option<ExecutionMode>,
    pub config_mode: Option<ExecutionMode>,
}

pub fn resolve_execution_mode(sources: ExecutionModeSources) -> ExecutionMode {
    sources
        .spec_mode
        .or(sources.agent_mode)
        .or(sources.config_mode)
        .unwrap_or_default()
}
