//! Shared model-to-preset contract.
pub use crate::presets::{ModelMetadata, resolve_preset_name};
#[cfg(test)]
mod tests {
 use super::*;
 use crate::settings::PromptPresetName;
 #[test]
 fn fable55_ids_resolve_to_launch_baseline() {
  for id in ["claude-fable-5-5", "claude-fable-5.5", "anthropic/claude-mythos-5-5"] {
   assert_eq!(resolve_preset_name(ModelMetadata {id,provider:"anthropic",name:None,prompt_preset:None},PromptPresetName::Auto),Some(PromptPresetName::ClaudeFable55));
  }
 }
 #[test]
 fn parts_reassemble_exact_provider_prompt_and_keep_project_source() {
  use maho_core::dynamic_prompt::{build::BuildDynamicSystemPromptOptions,PromptSurface};
  let preset=crate::presets::resolve_preset(ModelMetadata{id:"claude-fable-5-5",provider:"anthropic",name:None,prompt_preset:None},PromptPresetName::Auto,BuildDynamicSystemPromptOptions{append_system_prompt:None,custom_prompt:None,on_parts:None,cwd:"/work".into(),selected_tools:vec![],tool_snippets:Default::default(),prompt_guidelines:vec![],context_files:vec![maho_core::system_prompt::ContextFile{path:"/work/AGENTS.md".into(),content:"rules".into()}],skills:vec![],tuning_section:None,core_prompt:None,workstation_dialect:None,surface:PromptSurface::Terminal}).unwrap();
  assert_eq!(preset.parts.iter().map(|part|part.text.as_str()).collect::<String>(),preset.prompt);
  assert!(preset.parts.iter().any(|part|part.source.as_deref()==Some("/work/AGENTS.md")));
  assert_eq!(preset.parts[0].source.as_deref(),Some("prompt-preset:claude-fable-5-5"));
 }
}
