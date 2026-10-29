//! Skill content for `load_skills` (spec §3.2): a node's named skills are looked up once, at
//! node creation, and their `SKILL.md` body is prepended to the stored prompt.
use std::path::PathBuf;

/// Looks up the `SKILL.md` content of a skill by name.
pub trait SkillResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Option<String>;
}

/// Searches `<root>/<name>/SKILL.md` in each root, first match wins.
#[derive(Clone, Debug, Default)]
pub struct DirSkillResolver {
    pub roots: Vec<PathBuf>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
}

impl SkillResolver for DirSkillResolver {
    fn resolve(&self, name: &str) -> Option<String> {
        let name = name.trim();
        if !valid_name(name) {
            return None;
        }
        self.roots.iter().find_map(|root| {
            let text = std::fs::read_to_string(root.join(name).join("SKILL.md")).ok()?;
            Some(maho_skills::strip_front_matter(&text).trim().to_owned())
        })
    }
}

/// The prefix built from `skills` (in order, each once) and the diagnostics for missing ones.
pub fn skill_prefix(resolver: &dyn SkillResolver, node_id: &str, skills: &[String]) -> (String, Vec<String>) {
    let mut prefix = String::new();
    let mut diagnostics = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for skill in skills {
        let skill = skill.trim();
        if !seen.insert(skill.to_owned()) {
            continue;
        }
        match resolver.resolve(skill) {
            Some(content) => {
                prefix.push_str(&format!("# Skill: {skill}\n\n{content}\n\n"));
            }
            None => diagnostics.push(format!(
                "node \"{node_id}\": skill \"{skill}\" was not found; the node runs without it"
            )),
        }
    }
    if !prefix.is_empty() {
        prefix.push_str("# Task\n\n");
    }
    (prefix, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_strips_front_matter() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(dir.path().join("s")).expect("mkdir");
        std::fs::write(dir.path().join("s/SKILL.md"), "---\nname: s\n---\nBody").expect("write");
        let resolver = DirSkillResolver {
            roots: vec![dir.path().join("none"), dir.path().to_path_buf()],
        };
        assert_eq!(resolver.resolve("s").as_deref(), Some("Body"));
        assert_eq!(resolver.resolve("../s"), None);
        let (prefix, diagnostics) =
            skill_prefix(&resolver, "n", &["s".into(), "s".into(), "missing".into()]);
        assert_eq!(prefix, "# Skill: s\n\nBody\n\n# Task\n\n");
        assert_eq!(diagnostics.len(), 1);
    }
}
