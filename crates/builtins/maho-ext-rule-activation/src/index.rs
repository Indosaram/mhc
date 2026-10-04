use std::sync::Arc;
use maho_ext_api::{Extension, ExtensionApi, ExtensionFailure, EntryRendererOptions};
use crate::{renderer::render_rule_activation_entry, types::{RULE_ACTIVATION_ENTRY_TYPE, RuleActivationDetails}};
pub fn register_rule_activation_renderer(api: &mut ExtensionApi) { api.register_entry_renderer(RULE_ACTIVATION_ENTRY_TYPE, Arc::new(render_rule_activation_entry), EntryRendererOptions::default()); }
pub fn append_rule_activation(api: &ExtensionApi, details: &RuleActivationDetails) -> Result<(), ExtensionFailure> { api.append_entry(RULE_ACTIVATION_ENTRY_TYPE, Some(details.to_json())) }
#[derive(Default)]
pub struct RuleActivation;
impl Extension for RuleActivation { fn register(&self, api: &mut ExtensionApi) { register_rule_activation_renderer(api); } }
