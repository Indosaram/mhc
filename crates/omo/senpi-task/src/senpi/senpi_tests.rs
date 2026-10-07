use pretty_assertions::assert_eq;

use super::{SenpiThinkingLevel, as_senpi_thinking_level};

// senpi/thinking-level.test.ts

#[test]
fn every_senpi_thinking_level_passes_through_verbatim() {
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert_eq!(
            as_senpi_thinking_level(Some(level)).map(SenpiThinkingLevel::as_str),
            Some(level)
        );
    }
}

#[test]
fn omo_reasoning_none_maps_to_senpi_off() {
    assert_eq!(
        as_senpi_thinking_level(Some("none")),
        Some(SenpiThinkingLevel::Off)
    );
}

#[test]
fn unknown_variant_string_is_rejected() {
    assert_eq!(as_senpi_thinking_level(Some("ultra")), None);
    assert_eq!(as_senpi_thinking_level(Some("")), None);
    assert_eq!(as_senpi_thinking_level(Some("HIGH")), None);
}

#[test]
fn undefined_stays_undefined() {
    assert_eq!(as_senpi_thinking_level(None), None);
}

// senpi-api-tripwire.test.ts, mapped onto the host trait seams. The marker-extension boot and
// pinned npm artifact cases exercise the TypeScript host package itself (N/A in parity.md).

mod api_tripwire {
    use std::sync::Arc;

    use crate::agents::{
        AgentResolutionResult, BUILTIN_AGENT_DEFAULTS, ResolveAgentOptions, resolve_agent,
    };
    use crate::runners::in_process::child_options::{ChildResourceLoader, ChildSessionOptions};
    use crate::runners::in_process::runtime_fallback_settings::RetryFallbackSettings;
    use crate::runners::in_process::session_manager::ChildSessionManager;

    #[test]
    fn given_crate_root_when_curated_agent_exports_used_then_values_and_result_types_resolve() {
        let agents: Vec<_> = BUILTIN_AGENT_DEFAULTS
            .iter()
            .map(|definition| (definition.name.clone(), definition.clone()))
            .collect();
        let options = ResolveAgentOptions {
            model_override: Some("openai/explicit".to_string()),
        };

        let result = resolve_agent("explore", &agents, None, &options);

        assert_eq!(BUILTIN_AGENT_DEFAULTS.len(), 4);
        assert!(
            matches!(result, AgentResolutionResult::Resolved(_)),
            "{result:?}"
        );
    }

    #[test]
    fn given_host_session_seams_when_constructed_then_adapter_passes_session_options() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().into_owned();
        let sessions = dir.path().join("sessions");
        let session_manager = Arc::new(
            ChildSessionManager::create(&cwd, &sessions.to_string_lossy()).expect("session"),
        );

        let options = ChildSessionOptions {
            cwd,
            session_manager: Arc::clone(&session_manager),
            resource_loader: ChildResourceLoader::Minimal,
            system_prompt: None,
            custom_tools: Vec::new(),
            agent_dir: None,
            auth_storage: None,
            model_registry: None,
            model_runtime: None,
            model: None,
            thinking_level: None,
            settings: RetryFallbackSettings::default(),
            tools: Some(vec!["read".to_string(), "bash".to_string()]),
            exclude_tools: None,
        };

        assert!(Arc::ptr_eq(&options.session_manager, &session_manager));
        assert_eq!(options.resource_loader.extension_count(), 0);
        assert!(options.custom_tools.is_empty());
        assert_eq!(
            options.tools,
            Some(vec!["read".to_string(), "bash".to_string()])
        );
    }

    #[test]
    fn given_minimal_resource_loader_source_when_audited_then_marker_factory_absent() {
        let source = include_str!("../runners/in_process/child_options.rs");

        assert!(!source.contains("markerFactory"));
        assert!(!source.contains("marker_factory"));
    }
}
