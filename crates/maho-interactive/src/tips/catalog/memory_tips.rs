//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/memory-tips.ts`.

use super::types::TipDefinition;

pub const MEMORY_TIPS: &[TipDefinition] = &[
    TipDefinition { id: "memory.persistent", bindings: &[], requires_command: Some("memory"), render: |_k| format!("{} remembers across sessions. Close the terminal, come back tomorrow, and the context is still there.", maho_core::config::app_name()) },
    TipDefinition { id: "memory.remember-command", bindings: &[], requires_command: Some("remember"), render: |_k| "Use /remember to save something you want kept - a preference, a decision, a fact worth reusing.".to_string() },
    TipDefinition { id: "memory.just-tell-it", bindings: &[], requires_command: Some("remember"), render: |_k| "You can also just say it: \"remember that we deploy on Fridays\" is enough. No syntax, no file to open.".to_string() },
    TipDefinition { id: "memory.search-command", bindings: &[], requires_command: Some("search"), render: |_k| "Use /search to look through everything remembered so far, in plain words.".to_string() },
    TipDefinition { id: "memory.memory-command", bindings: &[], requires_command: Some("memory"), render: |_k| "Use /memory to see what is currently remembered about you and this project.".to_string() },
    TipDefinition { id: "memory.init-command", bindings: &[], requires_command: Some("init"), render: |_k| "Use /init to start memory for a new project, so it learns this codebase from the first session.".to_string() },
    TipDefinition { id: "memory.people-command", bindings: &[], requires_command: Some("people"), render: |_k| "Use /people to keep notes on teammates - who owns what, who to ask, what they prefer.".to_string() },
    TipDefinition { id: "memory.reflect-command", bindings: &[], requires_command: Some("reflect"), render: |_k| "Use /reflect to have the session reviewed now and the parts worth keeping written down.".to_string() },
    TipDefinition { id: "memory.dream-command", bindings: &[], requires_command: Some("dream"), render: |_k| "Memory tidies itself in the background between turns - /dream shows that work and lets you start it yourself.".to_string() },
    TipDefinition { id: "memory.sleeptime-command", bindings: &[], requires_command: Some("sleeptime"), render: |_k| "Use /sleeptime to decide how often memory reorganizes itself while you are not looking.".to_string() },
    TipDefinition { id: "memory.memfs-command", bindings: &[], requires_command: Some("memfs"), render: |_k| "Memory is plain files you own - browse them with /memfs and edit anything that looks wrong.".to_string() },
    TipDefinition { id: "memory.repository-command", bindings: &[], requires_command: Some("memory-repository"), render: |_k| "Every memory change is a git commit - use /memory-repository to see the history and undo a bad one.".to_string() },
    TipDefinition { id: "memory.doctor-command", bindings: &[], requires_command: Some("doctor"), render: |_k| "Use /doctor when memory feels off; it checks the setup and tells you what to fix.".to_string() },
    TipDefinition { id: "memory.facts-command", bindings: &[], requires_command: Some("facts"), render: |_k| "Use /facts to see the individual things learned about you, and drop the ones that no longer hold.".to_string() },
    TipDefinition { id: "memory.recompile-command", bindings: &[], requires_command: Some("recompile"), render: |_k| "Edited memory files by hand? Run /recompile so this session picks the changes up right away.".to_string() },
    TipDefinition { id: "memory.aha-moment", bindings: &[], requires_command: Some("memory"), render: |_k| "Memory speaks up on its own: when something remembered would change the next step, an Aha moment! line surfaces it mid-task. Silence means nothing relevant was found.".to_string() },
    TipDefinition { id: "memory.stop-repeating", bindings: &[], requires_command: Some("memory"), render: |_k| "Explain your setup once. Memory means you stop re-explaining it at the start of every session.".to_string() },
];
