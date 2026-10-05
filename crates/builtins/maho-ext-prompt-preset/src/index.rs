use std::sync::Arc;
use maho_ext_api::{BeforeAgentStartEventResult, BuildSystemPromptOptions, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent, ModelSelectEventResult};
use maho_core::{dynamic_prompt::build::BuildDynamicSystemPromptOptions, settings_manager::SettingsManager};
use crate::{presets::{ModelMetadata, resolve_preset}, settings::{PromptPresetName, load_prompt_preset_settings}};

#[derive(Default)]
pub struct PromptPreset;
impl Extension for PromptPreset {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::BeforeAgentStart, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(EventResult::None); };
            let Some(model) = &ctx.model else { return Ok(EventResult::None); };
            if has_user_system_prompt(&event.system_prompt_options) { return Ok(EventResult::None); }
            let Some(preset) = resolve_preset(metadata(model), settings(ctx), builder_input(&event.system_prompt_options, ctx)) else { return Ok(EventResult::None); };
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult { system_prompt: Some(with_user_appends(preset.prompt, &event.system_prompt_options)), ..Default::default() }))
        })));
        api.on(EventKind::ModelSelect, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::ModelSelect(event) = event else { return Ok(EventResult::None); };
            if has_user_system_prompt(&event.system_prompt_options) { return Ok(EventResult::ModelSelect(ModelSelectEventResult { system_prompt: Some(None), system_prompt_name: None })); }
            let preset = resolve_preset(metadata(&event.model), settings(ctx), builder_input(&event.system_prompt_options, ctx));
            let name = preset.as_ref().map(|preset| preset.name.as_str().into());
            Ok(EventResult::ModelSelect(ModelSelectEventResult { system_prompt: Some(preset.map(|preset| with_user_appends(preset.prompt, &event.system_prompt_options))), system_prompt_name: name }))
        })));
    }
}
fn metadata(model: &maho_ext_api::Model) -> ModelMetadata<'_> { ModelMetadata { id: &model.id, provider: model.provider.as_str(), name: Some(&model.name), prompt_preset: None } }
fn settings(ctx: &ExtensionContext) -> PromptPresetName {
    let home = std::env::var("HOME").unwrap_or_default();
    let settings = SettingsManager::create(&ctx.cwd.to_string_lossy(), &ctx.agent_dir.to_string_lossy(), &home, (ctx.is_project_trusted_fn)());
    load_prompt_preset_settings(settings.get_project().get("promptPreset").and_then(|value| value.as_str()), settings.get_global().get("promptPreset").and_then(|value| value.as_str()))
}
fn builder_input(options: &BuildSystemPromptOptions, ctx: &ExtensionContext) -> BuildDynamicSystemPromptOptions<'static> {
    BuildDynamicSystemPromptOptions {
        cwd: if options.cwd.as_os_str().is_empty() { ctx.cwd.to_string_lossy().into_owned() } else { options.cwd.to_string_lossy().into_owned() },
        selected_tools: options.tools.clone(), tool_snippets: Default::default(), prompt_guidelines: Vec::new(),
        context_files: options.context_files.iter().map(|file| maho_core::system_prompt::ContextFile { path: file.path.clone(), content: file.content.clone() }).collect(),
        skills: options.skills.iter().map(|skill| maho_core::skills::Skill {
            name: skill.name.clone(), description: skill.description.clone(), file_path: skill.file_path.clone(), base_dir: skill.base_dir.clone(), disable_model_invocation: skill.disable_model_invocation,
            source_info: maho_core::source_info::SourceInfo {
                path: skill.source_info.path.clone(), source: skill.source_info.source.clone(), base_dir: skill.source_info.base_dir.clone(),
                scope: match skill.source_info.scope { maho_ext_api::SourceScope::User => maho_core::source_info::SourceScope::User, maho_ext_api::SourceScope::Project => maho_core::source_info::SourceScope::Project, maho_ext_api::SourceScope::Temporary => maho_core::source_info::SourceScope::Temporary, maho_ext_api::SourceScope::System => maho_core::source_info::SourceScope::System },
                origin: match skill.source_info.origin { maho_ext_api::SourceOrigin::Package => maho_core::source_info::SourceOrigin::Package, maho_ext_api::SourceOrigin::TopLevel => maho_core::source_info::SourceOrigin::TopLevel },
            },
        }).collect(), tuning_section: None, core_prompt: None, workstation_dialect: None,
    }
}
pub fn has_user_system_prompt(options: &BuildSystemPromptOptions) -> bool { options.custom_prompt.as_deref().is_some_and(|prompt| !prompt.is_empty()) }
pub fn with_user_appends(prompt: String, options: &BuildSystemPromptOptions) -> String {
    match options.append_system_prompt.as_deref().filter(|append| !append.is_empty()) { Some(append) => format!("{prompt}\n\n{append}"), None => prompt }
}
