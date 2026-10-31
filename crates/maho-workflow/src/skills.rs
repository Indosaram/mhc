//! Skill content for `load_skills` (spec §3.2): a node's named skills are looked up once, at
//! node creation, and their `SKILL.md` body is prepended to the stored prompt.
use std::path::{Path, PathBuf};

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

/// The roots `load_skills` searches, in order: `<cwd>/.omo/skills`, `<cwd>/.agents/skills`, the
/// caller's `extra` roots, then the bundled root from the shared `maho-skills` resolver (validated
/// `MAHO_SKILLS_ROOT` override with its once-per-process notice, else the executable sibling;
/// Delta D2). Each root appears once.
pub fn skill_roots(
    cwd: &Path,
    extra: &[PathBuf],
    resolver: &maho_skills::RootResolver,
    notices: &maho_skills::NoticeContext<'_>,
) -> Vec<PathBuf> {
    let mut roots = vec![
        cwd.join(".omo").join("skills"),
        cwd.join(".agents").join("skills"),
    ];
    roots.extend(extra.iter().cloned());
    roots.extend(resolver.resolve(None, notices));
    let mut seen = std::collections::HashSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

/// The prefix built from `skills` (in order, each once) and the diagnostics for missing ones.
pub fn skill_prefix(
    resolver: &dyn SkillResolver,
    node_id: &str,
    skills: &[String],
) -> (String, Vec<String>) {
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

    /// Delta D2: an invalid (relative) override falls back to the executable-sibling root through
    /// the shared resolver, and its notice is reported at most once in the process.
    #[test]
    fn bundled_root_comes_from_the_shared_resolver() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(bin.join("skills/s")).expect("mkdir");
        std::fs::write(
            bin.join("skills/s/SKILL.md"),
            "---\nname: s\n---\nBundled body",
        )
        .expect("write");
        let resolver = maho_skills::RootResolver {
            env: std::sync::Arc::new(|key: &str| {
                (key == maho_skills::SKILLS_ROOT_ENV).then(|| "relative/skills".to_owned())
            }),
            exe: Some(bin.join("mhc")),
            ledger: std::sync::Arc::new(maho_skills::NoticeLedger::default()),
        };
        let logged = std::sync::Mutex::new(Vec::<String>::new());
        let log = |message: &str| logged.lock().expect("log").push(message.to_owned());
        let notices = maho_skills::NoticeContext {
            log: &log,
            ui: None,
        };
        let cwd = dir.path().join("work");
        for _ in 0..2 {
            // No extra roots: the bundled root can only come from the resolver.
            let roots = skill_roots(&cwd, &[], &resolver, &notices);
            assert_eq!(roots.last(), Some(&bin.join("skills")), "{roots:?}");
            assert_eq!(
                roots
                    .iter()
                    .filter(|root| **root == bin.join("skills"))
                    .count(),
                1
            );
            let (prefix, missing) = skill_prefix(&DirSkillResolver { roots }, "n", &["s".into()]);
            assert!(missing.is_empty());
            assert!(prefix.contains("Bundled body"), "{prefix}");
        }
        let logged = logged.lock().expect("log").clone();
        assert_eq!(logged.len(), 1, "{logged:?}");
        assert!(logged[0].contains(maho_skills::SKILLS_ROOT_ENV));
    }
}
