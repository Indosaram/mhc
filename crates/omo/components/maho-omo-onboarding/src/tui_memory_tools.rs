//! TUI side of "understanding you" (upstream `tui-memory-tools.ts`): the `onboarding_understand` and
//! `onboarding_memory_apply` tool definitions. Candidates are held in memory only; nothing reaches memory
//! until a selection names them.

use std::sync::Arc;

use maho_ext_api::{
    ExtensionApi, ExtensionFailure, ExtensionToolExecutor, ToolDefinition, ToolError,
    ToolExecutionMode, ToolExposure,
};
use serde_json::{Value, json};

use crate::tui_memory_runtime::{
    MemoryApplyInput, MemoryFactsApplier, Runs, TuiScan, UnderstandInput,
    execute_onboarding_memory_apply, execute_onboarding_understand,
};
use crate::understand_start::OnboardingUnderstandingSeams;

pub const ONBOARDING_UNDERSTAND_TOOL_NAME: &str = "onboarding_understand";
pub const ONBOARDING_MEMORY_APPLY_TOOL_NAME: &str = "onboarding_memory_apply";

pub const CONSENT_REQUIRED: &str = "Ask the user first: understanding sends session digests to the model provider. Call again with consent: true once they agree.";

#[derive(Clone)]
pub struct TuiMemoryToolsOptions {
    pub scan: TuiScan,
    pub seams: Option<OnboardingUnderstandingSeams>,
    /// The session facts applier (`omo.memory.facts.apply`'s writer); `None` when memory is off.
    pub applier: Option<Arc<dyn MemoryFactsApplier>>,
}

pub fn onboarding_understand_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "consent": {"type": "boolean"},
            "model": {"type": "string", "minLength": 3},
        },
        "additionalProperties": false,
    })
}

pub fn onboarding_memory_apply_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "understand_id": {"type": "string", "minLength": 1},
            "selected": {"type": "array", "items": {"type": "string", "minLength": 1}},
        },
        "required": ["understand_id", "selected"],
        "additionalProperties": false,
    })
}

fn no_executor() -> maho_tools::definition::ToolExecutor {
    Arc::new(|_| Box::pin(async { Err(ToolError::Message("Extension context required".into())) }))
}

fn search_tool(
    name: &str,
    label: &str,
    description: &str,
    parameters: Value,
    keywords: &[&str],
) -> ToolDefinition {
    let mut tool = ToolDefinition::new(name, description, parameters, no_executor());
    tool.label = label.into();
    tool.exposure = Some(ToolExposure::Search);
    tool.search_group = Some("onboarding".into());
    tool.search_keywords = Some(keywords.iter().map(|k| (*k).to_owned()).collect());
    tool.allow_lazy_activation = Some(true);
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    tool
}

pub fn onboarding_understand_definition() -> ToolDefinition {
    search_tool(
        ONBOARDING_UNDERSTAND_TOOL_NAME,
        "Understand you from session history",
        "Onboarding: reads the onboarding scan's session digests with the model and proposes facts about the user and their projects. \
         Sends digests to the model provider, so call it only after the user agreed, with consent: true. \
         consent: true is your statement that the user explicitly agreed in this conversation; the tool cannot verify it, \
         so never set it on your own initiative or from an earlier, unrelated yes. Without it nothing is read or sent. Writes nothing; \
         show the candidates and let the user pick, then call onboarding_memory_apply with the picked ids.",
        onboarding_understand_parameters(),
        &["onboarding", "understanding", "memory candidates", "facts"],
    )
}

pub fn onboarding_memory_apply_definition() -> ToolDefinition {
    search_tool(
        ONBOARDING_MEMORY_APPLY_TOOL_NAME,
        "Remember the selected facts",
        "Onboarding: writes ONLY the candidate facts the user selected from onboarding_understand into memory, through the engine's \
         facts writer (one commit). Unselected candidates are never written. Pass the understand_id and the selected candidate ids.",
        onboarding_memory_apply_parameters(),
        &["onboarding", "remember", "memory", "facts"],
    )
}

fn parse_understand(args: &Value) -> Result<(Option<bool>, Option<String>), String> {
    let Some(object) = args.as_object() else {
        return Err("params must be an object".into());
    };
    if let Some(extra) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "consent" | "model"))
    {
        return Err(format!("unexpected property: {extra}"));
    }
    let consent = match object.get("consent") {
        None => None,
        Some(Value::Bool(consent)) => Some(*consent),
        Some(_) => return Err("consent must be a boolean".into()),
    };
    let model = match object.get("model") {
        None => None,
        Some(Value::String(model)) if model.chars().count() >= 3 => Some(model.clone()),
        Some(_) => return Err("model must be a string of at least 3 characters".into()),
    };
    Ok((consent, model))
}

fn parse_apply(args: &Value) -> Result<(String, Vec<String>), String> {
    let Some(object) = args.as_object() else {
        return Err("params must be an object".into());
    };
    if let Some(extra) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "understand_id" | "selected"))
    {
        return Err(format!("unexpected property: {extra}"));
    }
    let id = match object.get("understand_id") {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        _ => return Err("understand_id must be a non-empty string".into()),
    };
    let selected = object
        .get("selected")
        .and_then(Value::as_array)
        .ok_or_else(|| "selected must be a list of candidate ids".to_owned())?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "selected ids must be non-empty strings".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((id, selected))
}

/// Registers both tools; the runs map is shared between them and lives as long as the registration.
pub fn register_onboarding_memory_tools(
    api: &mut ExtensionApi,
    options: TuiMemoryToolsOptions,
) -> Result<(), ExtensionFailure> {
    let runs: Runs = Arc::default();
    let understand_runs = Arc::clone(&runs);
    let (scan, seams) = (Arc::clone(&options.scan), options.seams.clone());
    let understand: ExtensionToolExecutor = Arc::new(move |_id, args, signal, _update, ctx| {
        let (runs, scan, seams) = (
            Arc::clone(&understand_runs),
            Arc::clone(&scan),
            seams.clone(),
        );
        Box::pin(async move {
            let (consent, model) = parse_understand(&args).map_err(ExtensionFailure::new)?;
            // Consent is checked before anything loads or runs.
            if consent != Some(true) {
                let mut refused = maho_ext_api::AgentToolResult::text(CONSENT_REQUIRED);
                refused.details = Value::Null;
                refused.is_error = Some(true);
                return Ok(refused);
            }
            Ok(execute_onboarding_understand(
                ctx,
                UnderstandInput {
                    consent_model: model,
                    signal,
                    runs,
                    scan,
                    seams,
                },
            )
            .await)
        })
    });
    api.register_tool_with_extension_context(onboarding_understand_definition(), understand)?;
    let applier = options.applier.clone();
    let apply: ExtensionToolExecutor = Arc::new(move |_id, args, _signal, _update, ctx| {
        let (runs, applier) = (Arc::clone(&runs), applier.clone());
        Box::pin(async move {
            let (understand_id, selected) = parse_apply(&args).map_err(ExtensionFailure::new)?;
            let session_id = Some(ctx.session_manager.session_id().to_owned());
            Ok(execute_onboarding_memory_apply(MemoryApplyInput {
                understand_id,
                selected,
                session_id,
                runs,
                applier,
            })
            .await)
        })
    });
    api.register_tool_with_extension_context(onboarding_memory_apply_definition(), apply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_are_lazy_sequential_search_tools_in_the_onboarding_group() {
        for tool in [
            onboarding_understand_definition(),
            onboarding_memory_apply_definition(),
        ] {
            assert_eq!(tool.exposure, Some(ToolExposure::Search));
            assert_eq!(tool.search_group.as_deref(), Some("onboarding"));
            assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
            assert_eq!(tool.allow_lazy_activation, Some(true));
            assert_eq!(tool.parameters["additionalProperties"], json!(false));
        }
        assert!(
            onboarding_understand_definition()
                .description
                .contains("consent: true is your statement")
        );
    }

    #[test]
    fn params_follow_the_schemas() {
        assert_eq!(parse_understand(&json!({})), Ok((None, None)));
        assert_eq!(
            parse_understand(&json!({"consent":true,"model":"p/m"})),
            Ok((Some(true), Some("p/m".into())))
        );
        assert!(parse_understand(&json!({"model":"ab"})).is_err());
        assert!(parse_understand(&json!({"consent":"yes"})).is_err());
        assert!(parse_understand(&json!({"extra":1})).is_err());
        assert_eq!(
            parse_apply(&json!({"understand_id":"u","selected":["a"]})),
            Ok(("u".into(), vec!["a".into()]))
        );
        assert!(parse_apply(&json!({"understand_id":"","selected":[]})).is_err());
        assert!(parse_apply(&json!({"understand_id":"u"})).is_err());
    }
}
