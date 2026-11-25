//! Port of senpi mcp/skill-reveal.ts (brain/hand S16c): skills-carry-MCP reveal state per execution scope.
//!
//! A runtime serving several scopes keeps one state per scope, so one scope's turn does not replace another's
//! skills and one scope's `/skill:<name>` does not use up another's reveal of the same skill. A runtime serving
//! one scope keeps one state, as before.
use crate::skills::{
    RegisteredSkillTool, SkillLike, SkillMcpDeclarations, parse_skill_mcp_declarations,
    skill_activation_targets,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct McpSkillRevealState {
    declarations: SkillMcpDeclarations,
    skills: BTreeMap<String, SkillLike>,
    loaded: BTreeSet<String>,
}

pub type SkillToolActivator = Arc<dyn Fn(&[String]) + Send + Sync>;

pub struct McpSkillReveal {
    states: Mutex<BTreeMap<String, McpSkillRevealState>>,
    registered: Arc<dyn Fn() -> Vec<RegisteredSkillTool> + Send + Sync>,
    activate: SkillToolActivator,
}

pub fn create_mcp_skill_reveal(
    registered: Arc<dyn Fn() -> Vec<RegisteredSkillTool> + Send + Sync>,
    activate: SkillToolActivator,
) -> McpSkillReveal {
    McpSkillReveal {
        states: Mutex::new(BTreeMap::new()),
        registered,
        activate,
    }
}

impl McpSkillReveal {
    /// The skills this scope's turn lists; its declarations are returned so the caller attaches their servers.
    pub fn start_turn(&self, scope: &str, skills: &[SkillLike]) -> SkillMcpDeclarations {
        let parsed = parse_skill_mcp_declarations(skills);
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = states.entry(scope.to_owned()).or_default();
        state.skills = skills
            .iter()
            .map(|skill| (skill.name.clone(), skill.clone()))
            .collect();
        let declarations = SkillMcpDeclarations {
            servers: parsed.servers.clone(),
            warnings: parsed.warnings.clone(),
        };
        state.declarations = parsed;
        declarations
    }

    /// The skills this scope's turn listed, to recognize a read of a skill's own file.
    pub fn skills(&self, scope: &str) -> Vec<(String, SkillLike)> {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(scope)
            .map(|state| {
                state
                    .skills
                    .iter()
                    .map(|(name, skill)| (name.clone(), skill.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Reveal one skill for this scope: a skill it never listed, or already revealed, reveals nothing again.
    pub fn reveal(&self, scope: &str, skill_name: &str) {
        let Some(declarations) = self.take_reveal(scope, skill_name) else {
            return;
        };
        let registered = (self.registered)();
        let targets = skill_activation_targets(&declarations, skill_name, &registered);
        if !targets.is_empty() {
            (self.activate)(&targets);
        }
    }

    /// Claim a skill reveal for `scope` without invoking `registered`/`activate` synchronously, so async
    /// callers can acquire their own lock to resolve registered tools and activate targets.
    pub fn take_reveal(&self, scope: &str, skill_name: &str) -> Option<SkillMcpDeclarations> {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = states.get_mut(scope)?;
        if state.loaded.contains(skill_name) || !state.skills.contains_key(skill_name) {
            return None;
        }
        state.loaded.insert(skill_name.to_owned());
        Some(SkillMcpDeclarations {
            servers: state.declarations.servers.clone(),
            warnings: state.declarations.warnings.clone(),
        })
    }
}
