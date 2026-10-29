//! Execution mode precedence (`manager/execution-mode.ts`).

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionMode {
    #[default]
    InProcess,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguredExecutionMode {
    Explicit(ExecutionMode),
    Auto,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct AutoExecutionModeSources {
    pub platform_is_windows: bool,
    pub process_runner_is_host: bool,
    pub has_session_context: bool,
    pub has_generation_handoff: bool,
}

pub const AUTO_HOST_CAPABILITIES: [&str; 2] = ["session_context", "generation_handoff"];

pub fn resolve_auto_execution_mode(sources: AutoExecutionModeSources) -> ExecutionMode {
    if !sources.platform_is_windows
        && sources.process_runner_is_host
        && sources.has_session_context
        && sources.has_generation_handoff
    {
        ExecutionMode::Process
    } else {
        ExecutionMode::InProcess
    }
}

pub fn resolve_configured_execution_mode(
    sources: ExecutionModeSources,
    configured: Option<ConfiguredExecutionMode>,
    auto_mode: Option<ExecutionMode>,
) -> ExecutionMode {
    let config_mode = match configured {
        Some(ConfiguredExecutionMode::Explicit(mode)) => Some(mode),
        Some(ConfiguredExecutionMode::Auto) => auto_mode,
        None => sources.config_mode,
    };
    sources
        .spec_mode
        .or(sources.agent_mode)
        .or(config_mode)
        .unwrap_or_default()
}

pub fn resolve_execution_mode(sources: ExecutionModeSources) -> ExecutionMode {
    sources
        .spec_mode
        .or(sources.agent_mode)
        .or(sources.config_mode)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_requires_host_runner_and_both_capabilities() {
        let ready = AutoExecutionModeSources {
            platform_is_windows: false,
            process_runner_is_host: true,
            has_session_context: true,
            has_generation_handoff: true,
        };
        assert_eq!(resolve_auto_execution_mode(ready), ExecutionMode::Process);
        assert_eq!(
            resolve_auto_execution_mode(AutoExecutionModeSources {
                has_generation_handoff: false,
                ..ready
            }),
            ExecutionMode::InProcess
        );
        assert_eq!(
            resolve_auto_execution_mode(AutoExecutionModeSources {
                platform_is_windows: true,
                ..ready
            }),
            ExecutionMode::InProcess
        );
    }

    #[test]
    fn explicit_modes_take_precedence_over_auto_resolution() {
        let sources = ExecutionModeSources {
            spec_mode: Some(ExecutionMode::InProcess),
            agent_mode: None,
            config_mode: None,
        };
        assert_eq!(
            resolve_configured_execution_mode(
                sources,
                Some(ConfiguredExecutionMode::Auto),
                Some(ExecutionMode::Process)
            ),
            ExecutionMode::InProcess
        );
    }
}
