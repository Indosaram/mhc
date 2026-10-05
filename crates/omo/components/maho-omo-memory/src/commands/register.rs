//! Barrel registrar for the memory slash-command suite.
//! Port of `components/memory/commands/register.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;

use super::doctor::register_doctor_command;
use super::dream::register_dream_command;
use super::facts::register_facts_command;
use super::init::register_init_command;
use super::memfs::register_memfs_command;
use super::memory::register_memory_command;
use super::memory_repository::register_memory_repository_command;
use super::people::register_people_command;
use super::recompile::register_recompile_command;
use super::reflect::register_reflect_command;
use super::remember::register_remember_command;
use super::search::register_search_command;
use super::sleeptime::register_sleeptime_command;

pub const MEMORY_COMMAND_NAMES: [&str; 13] = [
    "memory",
    "memfs",
    "remember",
    "init",
    "doctor",
    "recompile",
    "memory-repository",
    "sleeptime",
    "reflect",
    "dream",
    "search",
    "people",
    "facts",
];

pub fn register_memory_commands(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    register_memory_command(api, deps.clone());
    register_memfs_command(api, deps.clone());
    register_remember_command(api, deps.clone());
    register_init_command(api, deps.clone());
    register_doctor_command(api, deps.clone());
    register_recompile_command(api, deps.clone());
    register_memory_repository_command(api, deps.clone());
    register_sleeptime_command(api, deps.clone());
    register_reflect_command(api, deps.clone());
    register_dream_command(api, deps.clone());
    register_search_command(api, deps.clone());
    register_people_command(api, deps.clone());
    register_facts_command(api, deps);
}

pub use super::types::{
    CommandContext, CommandResponse, DreamCommandOutcome, DreamRequestSink,
    ManualDreamCommandRequest, ManualReflectionRequest, MemoryCommandDeps, MemoryCommandIdentity,
    MemoryCommandUi, NotifyLevel, ReflectionDisposition, ReflectionRequestReceipt,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;
    use maho_ext_api::{EventBus, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, NotificationType, SourceInfo};

    fn fresh_api() -> ExtensionApi {
        ExtensionApi::new(
            LoadedExtension::new("memory", std::path::PathBuf::from("/tmp"), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        )
    }

    #[test]
    fn given_a_fresh_extension_api_when_the_suite_registers_then_exactly_the_documented_commands_appear() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let mut api = fresh_api();

        register_memory_commands(&mut api, Arc::new(fake.deps.clone()));

        let mut names: Vec<String> = api.registered.commands.iter().map(|command| command.name.clone()).collect();
        names.sort();
        let mut expected: Vec<String> = MEMORY_COMMAND_NAMES.iter().map(|name| (*name).to_owned()).collect();
        expected.sort();
        assert_eq!(names, expected);
        assert_eq!(
            MEMORY_COMMAND_NAMES,
            [
                "memory", "memfs", "remember", "init", "doctor", "recompile", "memory-repository",
                "sleeptime", "reflect", "dream", "search", "people", "facts",
            ]
        );
    }

    #[test]
    fn given_the_registered_suite_when_each_registration_is_inspected_then_every_command_is_dispatchable() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let mut api = fresh_api();

        register_memory_commands(&mut api, Arc::new(fake.deps.clone()));

        for command in &api.registered.commands {
            assert!(
                MEMORY_COMMAND_NAMES.contains(&command.name.as_str()),
                "unexpected registered command: {}",
                command.name
            );
            assert!(
                Arc::strong_count(&command.handler) >= 1,
                "command {} has no handler",
                command.name
            );
        }
    }

    #[tokio::test]
    async fn given_a_bound_identity_with_a_repository_when_the_registered_memory_handler_runs_then_it_dispatches_through_the_shared_seams() {
        let (_root, identity) = temp_identity();
        seeded_repo(
            &identity,
            vec![seed("system/persona.md", "---\ndescription: Persona\n---\nbarrel wired\n")],
        );
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let mut api = fresh_api();
        register_memory_commands(&mut api, Arc::new(fake.deps.clone()));

        let handler = api
            .registered
            .commands
            .iter()
            .find(|command| command.name == "memory")
            .expect("memory command is registered")
            .handler
            .clone();
        let ui = Arc::new(RecordingUi::default());
        let context = extension_context(ui.clone());

        handler("", &context)
            .await
            .expect("registered memory handler succeeds");

        let last = ui.last_message().unwrap_or_default();
        assert!(last.contains("barrel wired"));
        assert_eq!(ui.last_level(), Some(NotificationType::Info));
    }
}
