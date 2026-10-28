//! Choosing target directories and the plan document (spec §2–3, §6 keys).

use crate::analyze::{Analysis, DirFacts};
use serde::Serialize;
use serde_json::Value;

/// `init_deep.*` settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub advise: bool,
    pub max_depth: usize,
    pub max_dirs: usize,
    pub min_files: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            advise: true,
            max_depth: 3,
            max_dirs: 12,
            min_files: 20,
        }
    }
}

/// Environment switch of the advisor; `0`, `false`, `off`, `no` turn it off.
pub const ADVICE_ENV: &str = "MAHO_CONTEXT_ADVICE";

impl Settings {
    /// From a merged config view (its `init_deep` object) and the environment.
    pub fn from_view(view: Option<&Value>, env: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut out = Self::default();
        if let Some(section) = view.and_then(|v| v.get("init_deep")) {
            if let Some(v) = section.get("advise").and_then(Value::as_bool) {
                out.advise = v;
            }
            let int = |key| {
                section
                    .get(key)
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
            };
            if let Some(v) = int("max_depth") {
                out.max_depth = v;
            }
            if let Some(v) = int("max_dirs") {
                out.max_dirs = v.max(1);
            }
            if let Some(v) = int("min_files") {
                out.min_files = v.max(1);
            }
        }
        if env(ADVICE_ENV).is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        }) {
            out.advise = false;
        }
        out
    }

    /// For `cwd`: the native harness view of `omo.jsonc` and the process environment.
    pub fn load(cwd: &std::path::Path) -> Self {
        let env = maho_config::process_env();
        let loaded = maho_config::load_native(cwd, &env);
        Self::from_view(Some(&loaded.view), &|key| std::env::var(key).ok())
    }
}

/// What happens to a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Keep,
}

/// One directory that gets a context file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Target {
    pub path: String,
    pub depth: usize,
    pub action: Action,
    pub files: usize,
    pub subtree_code_files: usize,
    pub languages: Vec<String>,
    pub manifests: Vec<String>,
    pub entry_points: Vec<String>,
    pub has_tests: bool,
    /// Targets directly below this one (nearest target ancestor relation).
    pub children: Vec<String>,
    #[serde(skip)]
    pub existing: Option<String>,
}

/// The plan of one root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub root: String,
    /// Deepest first.
    pub targets: Vec<Target>,
}

fn score(facts: &DirFacts) -> usize {
    facts.subtree_code_files + 40 * facts.manifests.len() + 10 * usize::from(facts.has_tests)
}

fn is_ancestor(ancestor: &str, path: &str) -> bool {
    ancestor == "." || path.starts_with(&format!("{ancestor}/"))
}

/// The chosen directories (relative paths), root first, then by score.
pub fn choose(analysis: &Analysis, settings: &Settings) -> Vec<String> {
    let mut candidates: Vec<&DirFacts> = analysis
        .dirs
        .values()
        .filter(|f| f.path != ".")
        .filter(|f| f.depth <= settings.max_depth)
        .filter(|f| !f.manifests.is_empty() || f.subtree_code_files >= settings.min_files)
        // Grouping directories (no files of their own, e.g. `crates/`) leave it to their children.
        .filter(|f| f.files > 0)
        .collect();
    // Single-child collapse: drop a candidate whose code all sits in exactly one candidate child.
    let paths: Vec<String> = candidates.iter().map(|f| f.path.clone()).collect();
    let manifest_parent_holds_all = |f: &DirFacts| {
        let parent = f.path.rsplit_once('/').map(|(parent, _)| parent);
        parent.is_some_and(|parent| {
            paths.iter().any(|p| p == parent)
                && analysis.dirs.get(parent).is_some_and(|p| {
                    !p.manifests.is_empty() && p.subtree_code_files == f.subtree_code_files
                })
        })
    };
    candidates.retain(|f| {
        if !f.manifests.is_empty() {
            return true;
        }
        // A package's only code directory (e.g. `src/` holding all of it) adds nothing.
        if manifest_parent_holds_all(f) {
            return false;
        }
        let children: Vec<&String> = paths
            .iter()
            .filter(|p| p.rsplit_once('/').map(|(parent, _)| parent) == Some(f.path.as_str()))
            .collect();
        !(children.len() == 1
            && analysis
                .dirs
                .get(children[0])
                .is_some_and(|child| child.subtree_code_files == f.subtree_code_files))
    });
    candidates.sort_by(|a, b| {
        score(b)
            .cmp(&score(a))
            .then(a.depth.cmp(&b.depth))
            .then(a.path.cmp(&b.path))
    });
    let mut out = vec![".".to_owned()];
    out.extend(
        candidates
            .into_iter()
            .take(settings.max_dirs.saturating_sub(1))
            .map(|f| f.path.clone()),
    );
    out
}

/// Builds the plan. `existing` reads a context file's text (for `update`).
pub fn plan(
    analysis: &Analysis,
    settings: &Settings,
    update: bool,
    existing: &dyn Fn(&str, &str) -> Option<String>,
) -> Plan {
    let chosen = choose(analysis, settings);
    let mut targets: Vec<Target> = chosen
        .iter()
        .filter_map(|path| analysis.dirs.get(path))
        .map(|facts| {
            let action = match (&facts.context_file, update) {
                (None, _) => Action::Create,
                (Some(_), true) => Action::Update,
                (Some(_), false) => Action::Keep,
            };
            let children = chosen
                .iter()
                .filter(|other| {
                    *other != &facts.path
                        && is_ancestor(&facts.path, other)
                        && !chosen.iter().any(|mid| {
                            mid != &facts.path
                                && mid != *other
                                && is_ancestor(&facts.path, mid)
                                && is_ancestor(mid, other)
                        })
                })
                .cloned()
                .collect();
            Target {
                path: facts.path.clone(),
                depth: facts.depth,
                action,
                files: facts.files,
                subtree_code_files: facts.subtree_code_files,
                languages: facts.top_languages(),
                manifests: facts.manifests.clone(),
                entry_points: facts.entry_points.clone(),
                has_tests: facts.has_tests,
                children,
                existing: match action {
                    Action::Update => facts
                        .context_file
                        .as_deref()
                        .and_then(|name| existing(&facts.path, name)),
                    _ => None,
                },
            }
        })
        .collect();
    targets.sort_by(|a, b| b.depth.cmp(&a.depth).then(a.path.cmp(&b.path)));
    Plan {
        root: analysis.root.to_string_lossy().into_owned(),
        targets,
    }
}

/// A plain-text table of the plan.
pub fn table(plan: &Plan) -> String {
    let mut lines = vec![format!("root: {}", plan.root)];
    for t in &plan.targets {
        lines.push(format!(
            "{:<7} {}  ({} code files{}{})",
            format!("{:?}", t.action).to_lowercase(),
            t.path,
            t.subtree_code_files,
            if t.languages.is_empty() {
                String::new()
            } else {
                format!(", {}", t.languages.join("/"))
            },
            if t.manifests.is_empty() {
                String::new()
            } else {
                format!(", {}", t.manifests.join(" "))
            },
        ));
    }
    lines.join("\n")
}
