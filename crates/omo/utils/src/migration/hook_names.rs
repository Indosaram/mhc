#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookRename {
    Renamed(&'static str),
    Removed,
}

pub const HOOK_NAME_MAP: [(&str, HookRename); 8] = [
    (
        "anthropic-auto-compact",
        HookRename::Renamed("anthropic-context-window-limit-recovery"),
    ),
    ("sisyphus-orchestrator", HookRename::Renamed("atlas")),
    (
        "sisyphus-gpt-hephaestus-reminder",
        HookRename::Renamed("no-sisyphus-gpt"),
    ),
    ("empty-message-sanitizer", HookRename::Removed),
    ("delegate-task-english-directive", HookRename::Removed),
    ("gpt-permission-continuation", HookRename::Removed),
    ("thinking-block-validator", HookRename::Removed),
    ("session-recovery", HookRename::Removed),
];

pub fn hook_name_mapping(hook: &str) -> Option<HookRename> {
    HOOK_NAME_MAP
        .iter()
        .find(|(name, _)| *name == hook)
        .map(|(_, mapping)| *mapping)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookMigration {
    pub migrated: Vec<String>,
    pub changed: bool,
    pub removed: Vec<String>,
}

pub fn migrate_hook_names<S: AsRef<str>>(hooks: &[S]) -> HookMigration {
    let mut result = HookMigration {
        migrated: Vec::new(),
        changed: false,
        removed: Vec::new(),
    };
    for hook in hooks.iter().map(AsRef::as_ref) {
        match hook_name_mapping(hook) {
            Some(HookRename::Removed) => {
                result.removed.push(hook.to_string());
                result.changed = true;
            }
            Some(HookRename::Renamed(new_name)) => {
                result.changed |= new_name != hook;
                result.migrated.push(new_name.to_string());
            }
            None => result.migrated.push(hook.to_string()),
        }
    }
    result
}
