//! Port of `omo-senpi/src/components/skill-commands/index.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! Makes the bare `/<bundled-skill> args` form that omo's docs, skills, and plan handoffs tell users
//! to type behave exactly like `/skill:<name> args`.
//!
//! The rewrite happens in the input event, ahead of every other omo input handler, instead of in a
//! registered command: a command handler could only re-submit through `sendUserMessage`, whose
//! `extension` source would drop the human provenance the ulw-plan gate, ultrawork, skill pointers,
//! and continuation resets key on. Rewritten here, the submission keeps its `interactive`/`rpc`
//! source and every downstream handler sees the canonical `/skill:` form before senpi expands it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use maho_ext_api::{
    AutocompleteProviderFactory, ComponentLogger, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionContext, ExtensionEvent, ExtensionFailure, InputEventResult, InputSource, NotificationType,
};
use maho_omo_bundled_skills::{HostCommands, host_commands_from_runtime, resolve_bundled_skills_dir};

use crate::autocomplete::wrap_with_bare_skill_commands;
use crate::bare_skill_command::{BareSkillCommandResolution, read_bundled_skill_names, resolve_bare_skill_command};

pub const SKILL_COMMANDS_COMPONENT_NAME: &str = "skill-commands";

#[derive(Clone, Default)]
pub struct SkillCommandsComponentOptions {
    /// Overrides the packaged skills root; tests point it at a fixture.
    pub skills_dir: Option<PathBuf>,
    /// Upstream `process.env`, used to resolve the packaged skills root.
    pub env: Option<BTreeMap<String, String>>,
    /// Shared component logger supplied by the composition.
    pub logger: Option<Arc<dyn ComponentLogger>>,
}

#[derive(Clone, Default)]
pub struct SkillCommandsComponent {
    pub options: SkillCommandsComponentOptions,
}

impl SkillCommandsComponent {
    pub const NAME: &'static str = SKILL_COMMANDS_COMPONENT_NAME;

    pub fn new(options: SkillCommandsComponentOptions) -> Self {
        Self { options }
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Self {
        Self { options: SkillCommandsComponentOptions { env: Some(env.clone()), ..Default::default() } }
    }
}

impl Extension for SkillCommandsComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let env = self.options.env.clone().unwrap_or_else(process_env);
        let skills_dir = self.options.skills_dir.clone().or_else(|| resolve_bundled_skills_dir(&env));
        let bundled_skill_names = match read_bundled_skill_names(skills_dir.as_deref()) {
            Ok(names) => names,
            Err(error) => {
                log(&self.options.logger, "bundled skills directory could not be read", Some(serde_json::json!({ "error": error.to_string() })));
                return;
            }
        };
        if bundled_skill_names.is_empty() {
            log(&self.options.logger, "no bundled skills found; bare skill commands disabled", None);
            return;
        }

        let host_commands = host_commands_from_runtime(api.runtime.clone());
        let input_names = bundled_skill_names.clone();
        let input_commands = host_commands.clone();
        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let result = input_result(event, &input_names, &input_commands, ctx);
                Box::pin(async move { result })
            }),
        );

        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                if ctx.has_ui {
                    let factory: AutocompleteProviderFactory = autocomplete_factory(bundled_skill_names.clone(), host_commands.clone());
                    let _ = ctx.ui.add_autocomplete_provider(factory);
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
    }
}

fn input_result(
    event: &ExtensionEvent,
    bundled_skill_names: &BTreeSet<String>,
    host_commands: &HostCommands,
    ctx: &ExtensionContext,
) -> Result<EventResult, ExtensionFailure> {
    let ExtensionEvent::Input(input) = event else { return Ok(EventResult::None) };
    if input.source == InputSource::Extension {
        return Ok(EventResult::Input(InputEventResult::Continue));
    }
    let commands = (host_commands)()?;
    match resolve_bare_skill_command(&input.text, bundled_skill_names, commands.as_deref()) {
        BareSkillCommandResolution::NotBareSkill | BareSkillCommandResolution::Shadowed => {
            Ok(EventResult::Input(InputEventResult::Continue))
        }
        BareSkillCommandResolution::Unavailable { name } => {
            if ctx.has_ui {
                ctx.ui.notify(
                    &format!("/{name} is unavailable: the {name} skill is disabled (disabled_skills) or not loaded."),
                    NotificationType::Warning,
                );
            }
            Ok(EventResult::Input(InputEventResult::Handled))
        }
        BareSkillCommandResolution::Expand { text } => {
            Ok(EventResult::Input(InputEventResult::Transform { text, images: input.images.clone() }))
        }
    }
}

fn autocomplete_factory(bundled_skill_names: BTreeSet<String>, host_commands: HostCommands) -> AutocompleteProviderFactory {
    Arc::new(move |current| wrap_with_bare_skill_commands(current, bundled_skill_names.clone(), host_commands.clone()))
}

fn log(logger: &Option<Arc<dyn ComponentLogger>>, message: &str, details: Option<maho_ext_api::JsonValue>) {
    if let Some(logger) = logger {
        let details = details.unwrap_or_else(|| serde_json::json!({ "component": SKILL_COMMANDS_COMPONENT_NAME }));
        logger.info(message, Some(&details));
    }
}

fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}
