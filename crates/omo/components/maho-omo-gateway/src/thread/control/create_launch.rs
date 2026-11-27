//! Launch profile validation & permission preset narrowing for `thread_create` (`thread/control/create-launch.ts`).

use serde_json::{Value, json};

use super::super::contracts::params::ThreadCreateInput;
use super::super::contracts::permission_presets::{PermissionPreset, preset_within};
use super::super::tools::internals::failure;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchProfile {
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CreateLaunchPlan {
    Ok {
        permission_preset: Option<String>,
        profile: LaunchProfile,
    },
    Error(Value),
}

pub fn create_launch(value: &ThreadCreateInput, creator: Option<&str>) -> CreateLaunchPlan {
    let mut named = Vec::new();
    if value.model.is_some() {
        named.push("model");
    }
    if value.reasoning.is_some() {
        named.push("reasoning");
    }
    if value.permission_preset.is_some() {
        named.push("permission_preset");
    }
    if value.thread.is_some() && !named.is_empty() {
        return CreateLaunchPlan::Error(failure(
            "invalid_arguments",
            format!(
                "{} apply to a new thread; thread opens an existing one as it is.",
                named.join(", ")
            ),
            "Drop them to open the thread, or drop thread to create a new one with them.",
            Some(json!({ "fields": named })),
        ));
    }
    if value.delivery.is_some() && value.prompt.is_none() {
        return CreateLaunchPlan::Error(failure(
            "invalid_arguments",
            "delivery names how prompt is delivered; no prompt was given.",
            "Pass prompt, or drop delivery.",
            None,
        ));
    }
    let mut profile = LaunchProfile {
        thinking_level: value.reasoning.clone(),
        ..Default::default()
    };
    if let Some(model) = value.model.as_deref() {
        let Some(slash) = model.find('/') else {
            return CreateLaunchPlan::Error(failure(
                "invalid_arguments",
                format!("model {:?} is not a provider/id.", model),
                "Pass the exact provider/id the device lists (thread_set_model, tool.thread_set_model() in an eval cell, shows them).",
                Some(json!({ "model": model })),
            ));
        };
        if slash == 0 || slash == model.len() - 1 {
            return CreateLaunchPlan::Error(failure(
                "invalid_arguments",
                format!("model {:?} is not a provider/id.", model),
                "Pass the exact provider/id the device lists (thread_set_model, tool.thread_set_model() in an eval cell, shows them).",
                Some(json!({ "model": model })),
            ));
        }
        profile.provider = Some(model[..slash].to_owned());
        profile.model_id = Some(model[slash + 1..].to_owned());
    }
    let asked = value.permission_preset.as_deref();
    if let Some(asked) = asked {
        let allowed =
            PermissionPreset::parse(asked).is_some_and(|parsed| preset_within(parsed, creator));
        if !allowed {
            let mut details = json!({
                "reason": "permission_preset_wider",
                "requested": asked,
            });
            if let Some(c) = creator
                && let Some(obj) = details.as_object_mut()
            {
                obj.insert("creator".into(), Value::String(c.to_owned()));
            }
            return CreateLaunchPlan::Error(failure(
                "scope_denied",
                format!(
                    "This session runs in {}; a thread it creates cannot run in {asked}, which allows more.",
                    creator.unwrap_or("an unknown approval mode")
                ),
                "Pass this session's own mode or a narrower one (ask is always allowed), or leave permission_preset unset.",
                Some(details),
            ));
        }
    }
    let permission_preset = asked.or(creator).map(str::to_owned);
    CreateLaunchPlan::Ok {
        permission_preset,
        profile,
    }
}
