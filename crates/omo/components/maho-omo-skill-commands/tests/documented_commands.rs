//! Port of `omo-senpi/src/components/skill-commands/documented-commands.test.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! Every slash command omo tells a Native user to type must dispatch: a bundled skill, a native
//! builtin, or an omo-registered command. The scan reads the SELECTED shipped skill assets and the
//! repo README, and the known set is resolved from the real registries - native discovery, the
//! native builtin slash-command list, and the omo components' own registration API - never a name
//! list.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_core::slash_commands::builtin_slash_commands;
use maho_ext_api::*;
use maho_omo_skill_commands::read_bundled_skill_names;
use regex::Regex;

/// The selected overlay is the packaging lane's deliverable. A checkout that has not staged it must
/// fail loudly rather than pass on a substitute.
const SELECTED_SKILLS_ENV: &str = "MAHO_OMO_LATEST_SKILLS_ROOT";

fn selected_assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../assets/omo-latest")
}

fn skills_root() -> PathBuf {
    std::env::var(SELECTED_SKILLS_ENV).map(PathBuf::from).unwrap_or_else(|_| selected_assets_root().join("skills"))
}

fn require_skills_root() -> PathBuf {
    let root = skills_root();
    assert!(root.is_dir(), "the selected latest skill assets are required at {} (override with {SELECTED_SKILLS_ENV}); stage the packaging overlay first", root.display());
    root
}

/// Upstream `NATIVE_GUIDES = ["docs/guide/overview.md", "docs/guide/orchestration.md", "README.md"]`.
/// The native repo ships no `docs/guide/`, so the README is the one guide file that exists; it is
/// read unconditionally, so a missing README fails the scan instead of shrinking it silently.
fn guide_files() -> Vec<PathBuf> {
    vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..").join("README.md")]
}

fn read_required(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("required shipped file {} could not be read: {error}", path.display()))
}

/// Upstream `DOCUMENTED_COMMAND = /(?:^|[\s(|"'>])`\/([a-z][a-z0-9-]*)(?=[\s`])/gm`.
///
/// Rust's `regex` has no lookaround, so the trailing lookahead becomes an equivalent consumed
/// delimiter and the name stays in capture 1. `(?m)` keeps upstream's `m` flag so `^` also matches
/// at a line start; against the shipped assets both forms select the same names.
fn documented_commands(root: &Path) -> BTreeSet<String> {
    let pattern = Regex::new(r#"(?m)(?:^|[\s(|"'>])`/([a-z][a-z0-9-]*)([\s`]|$)"#).expect("documented-command regex");
    let mut found = BTreeSet::new();
    for name in read_bundled_skill_names(Some(root)).expect("bundled skill names") {
        collect_documented(&pattern, &read_required(&root.join(&name).join("SKILL.md")), &mut found);
    }
    for path in guide_files() {
        collect_documented(&pattern, &read_required(&path), &mut found);
    }
    found
}

fn collect_documented(pattern: &Regex, source: &str, found: &mut BTreeSet<String>) {
    for capture in pattern.captures_iter(source) {
        found.insert(capture[1].to_owned());
    }
}

/// Upstream `omoCommandNames()` walks `packages/omo-senpi/src` for every `registerCommand(...)`.
/// The native omo command surface is exactly the `task` and `memory` components (the upstream
/// `computer-use` and `memory/palace` owners have no native crate yet), so both are registered here
/// through their own public registration API and the names are read back from the real
/// `ExtensionApi` registry: dropping a `register_command` call in either owner makes this set - and
/// the dispatch assertion below - fail.
fn omo_registered_commands() -> BTreeSet<String> {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("omo-command-owners", PathBuf::from(env!("CARGO_MANIFEST_DIR")), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_omo_task::commands::register_task_commands(&mut api, Arc::new(NoTasks));
    maho_omo_task::dag_commands::register_dag_commands(&mut api, Arc::new(NoDags));
    maho_omo_memory::commands::register::register_memory_commands(&mut api, Arc::new(memory_command_deps()));
    api.registered.commands.iter().map(|command| command.name.clone()).collect()
}

struct NoTasks;

impl maho_omo_task::commands::CommandManager for NoTasks {
    fn list(&self, _: &senpi_task::manager::types::ListScope) -> Vec<senpi_task::state::TaskRecord> {
        Vec::new()
    }

    fn cancel_task(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
}

struct NoDags;

impl maho_omo_task::dag_commands::DagCommandManager for NoDags {
    fn list(&self, _: &str, _: usize) -> Result<Vec<senpi_task::dag::manager::DagRunSummary>, ExtensionFailure> {
        Ok(Vec::new())
    }

    fn snapshot(&self, _: &str, _: &str) -> Option<senpi_task::dag::types::DagRunSnapshot> {
        None
    }

    fn task_record(&self, _: &str) -> Option<senpi_task::state::TaskRecord> {
        None
    }
}

struct NoActions;

impl ExtensionActions for NoActions {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> {
        Ok(())
    }

    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> {
        Ok(())
    }

    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }

    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
}

/// The memory command suite reads its deps only when a command runs, so registration needs the seam
/// set and nothing else.
fn memory_command_deps() -> maho_omo_memory::commands::types::MemoryCommandDeps {
    maho_omo_memory::commands::types::MemoryCommandDeps {
        resolve_context: Arc::new(|_: &str| -> Option<maho_omo_memory::context::MemoryIdentityContext> { None }),
        resolve_identity: None,
        settings: Arc::new(|| Ok(serde_json::json!({}))),
        bust_prompt_cache: Arc::new(|| {}),
        config_path: None,
        full_config: None,
        actions: Arc::new(NoActions),
        prompt: Arc::new(maho_omo_memory::prompt::MemoryPromptHandler::default()),
        sessions_dir: PathBuf::new(),
        reflect: None,
        dream: None,
        facts_retry: None,
        exec: None,
        env: None,
        people_ask: None,
        now: None,
        is_process_alive: None,
    }
}

fn known_commands(root: &Path) -> BTreeSet<String> {
    let mut known: BTreeSet<String> = read_bundled_skill_names(Some(root)).expect("bundled skill names");
    known.extend(builtin_slash_commands().iter().map(|command| command.name.to_owned()));
    known.extend(omo_registered_commands());
    known
}

#[test]
fn given_the_shipped_skills_and_guides_when_they_name_a_slash_command_then_that_command_dispatches() {
    let root = require_skills_root();
    let known = known_commands(&root);

    let unregistered: Vec<String> = documented_commands(&root).into_iter().filter(|command| !known.contains(command)).collect();

    assert!(unregistered.is_empty(), "documented but not dispatchable: {unregistered:?}");
}

#[test]
fn given_the_scan_inputs_when_resolved_then_each_source_is_populated_so_an_empty_set_cannot_pass_vacuously() {
    let root = require_skills_root();
    let bundled: BTreeSet<String> = read_bundled_skill_names(Some(&root)).expect("bundled skill names");
    let builtin: BTreeSet<String> = builtin_slash_commands().iter().map(|command| command.name.to_owned()).collect();
    let omo = omo_registered_commands();
    let documented = documented_commands(&root);

    assert!(bundled.contains("ulw-execute"));
    assert!(builtin.contains("model") && builtin.contains("login"));
    assert!(omo.contains("tasks") && omo.contains("dag"));
    assert!(documented.contains("ulw-execute"));
}
