//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "argument_hints.rs"]
mod argument_hints;
#[path = "autocomplete.rs"]
mod autocomplete;
#[path = "bare_skill_command.rs"]
mod bare_skill_command;
#[path = "documented_commands.rs"]
mod documented_commands;
#[path = "skill_commands.rs"]
mod skill_commands;
