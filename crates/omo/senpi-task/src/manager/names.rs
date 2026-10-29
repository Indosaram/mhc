//! Per-parent task name reservation (`manager/names.ts`).

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRegistration {
    pub name: String,
    pub warning: Option<String>,
}

#[derive(Debug, Default)]
pub struct NameRegistry {
    by_parent: HashMap<String, HashSet<String>>,
}

fn normalize(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|trimmed| !trimmed.is_empty())
}

impl NameRegistry {
    pub fn register(
        &mut self,
        parent_session_id: &str,
        requested: Option<&str>,
        fallback: Option<&str>,
    ) -> NameRegistration {
        let taken = self
            .by_parent
            .entry(parent_session_id.to_string())
            .or_default();
        let desired = normalize(requested)
            .or(normalize(fallback))
            .map_or_else(|| format!("task-{}", taken.len() + 1), str::to_string);
        if taken.insert(desired.clone()) {
            return NameRegistration {
                name: desired,
                warning: None,
            };
        }
        let mut suffix = 2;
        while taken.contains(&format!("{desired}-{suffix}")) {
            suffix += 1;
        }
        let resolved = format!("{desired}-{suffix}");
        taken.insert(resolved.clone());
        NameRegistration {
            warning: Some(format!(
                "Task name \"{desired}\" already exists in this session; using \"{resolved}\"."
            )),
            name: resolved,
        }
    }

    pub fn is_available(&self, parent_session_id: &str, name: &str) -> bool {
        !self
            .by_parent
            .get(parent_session_id)
            .is_some_and(|taken| taken.contains(name))
    }

    pub fn release(&mut self, parent_session_id: &str, name: &str) {
        if let Some(taken) = self.by_parent.get_mut(parent_session_id) {
            taken.remove(name);
        }
    }
}
