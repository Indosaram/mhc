//! Task id allocation (ported from oh-my-pi `task/output-manager.ts` `AgentOutputManager`): a
//! requested name is used verbatim the first time, repeats get `-2`, `-3`, ...; ids already present
//! in the state directory are reserved so a resumed session never overwrites earlier output.
use std::collections::BTreeSet;
use std::path::Path;

/// Longest generated name.
pub const MAX_NAME_LEN: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct IdAllocator {
    taken: BTreeSet<String>,
}

impl IdAllocator {
    /// Seeds the taken set from `<dir>/*.json` records.
    pub fn seeded_from(dir: &Path) -> Self {
        let mut allocator = Self::default();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".json") {
                    allocator.taken.insert(stem.to_owned());
                }
            }
        }
        allocator
    }

    /// Reserves ids allocated elsewhere.
    pub fn reserve<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.taken.extend(ids);
    }

    /// The first free id among `base`, `base-2`, `base-3`, ...
    pub fn allocate(&mut self, base: &str) -> String {
        let base = sanitize(base);
        let mut candidate = base.clone();
        let mut n = 2;
        while self.taken.contains(&candidate) {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        self.taken.insert(candidate.clone());
        candidate
    }
}

/// Keeps `[A-Za-z0-9_-]`, at most [`MAX_NAME_LEN`] characters; empty becomes `Task`.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(MAX_NAME_LEN)
        .collect();
    if cleaned.is_empty() {
        "Task".to_owned()
    } else {
        cleaned
    }
}

/// A CamelCase name from the first words of the task text (for calls without `name`).
pub fn name_from_task(task: &str) -> String {
    let mut name = String::new();
    for word in task
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
    {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
            name.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
        if name.len() >= MAX_NAME_LEN {
            break;
        }
    }
    sanitize(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_names_get_suffixes() {
        let mut ids = IdAllocator::default();
        assert_eq!(ids.allocate("Anna"), "Anna");
        assert_eq!(ids.allocate("Anna"), "Anna-2");
        assert_eq!(ids.allocate("Anna"), "Anna-3");
        assert_eq!(ids.allocate("a b/c"), "abc");
        assert_eq!(ids.allocate(""), "Task");
    }

    #[test]
    fn seeds_from_records() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("Fix.json"), "{}").expect("write");
        std::fs::write(dir.path().join("Fix.md"), "").expect("write");
        let mut ids = IdAllocator::seeded_from(dir.path());
        assert_eq!(ids.allocate("Fix"), "Fix-2");
    }

    #[test]
    fn names_from_task_text() {
        assert_eq!(name_from_task("fix the login bug in auth.rs"), "FixTheLoginBug");
        assert_eq!(name_from_task("!!!"), "Task");
    }
}
