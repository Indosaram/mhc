use std::collections::{BTreeMap, BTreeSet};

use memory_core::git::{GitExec, GitExecOptions, GitMemoryRepo};
use memory_core::memfs::parse_memory_file;
use serde::Serialize;

use super::PalaceError;

pub const UNCOMMITTED_LABEL: &str = "uncommitted - not active in system prompt";

const GIT_TIMEOUT_MS: u64 = 30_000;
const MEMORY_DIR_TOKEN: &str = "$MEMORY_DIR";
const SYSTEM_PREFIX: &str = "system/";
const SKILLS_PREFIX: &str = "skills/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum PalaceEntryState {
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "uncommitted - not active in system prompt")]
    Uncommitted,
}

impl PalaceEntryState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Uncommitted => UNCOMMITTED_LABEL,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceCoreEntry {
    pub path: String,
    pub projection: String,
    pub description: String,
    pub body: String,
    pub state: PalaceEntryState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceExternalEntry {
    pub path: String,
    pub binary: bool,
    pub byte_size: u64,
    pub state: PalaceEntryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

struct Measured {
    binary: bool,
    byte_size: u64,
    text: Option<String>,
}

pub fn collect_core(
    repo: &GitMemoryRepo,
    head: Option<&str>,
) -> Result<Vec<PalaceCoreEntry>, PalaceError> {
    let committed = match head {
        Some(head) => repo
            .ls_tree(Some(head), None)?
            .into_iter()
            .filter(|path| is_system_markdown(path.as_str()))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let dirty = dirty_paths(repo);
    let mut paths = committed;
    paths.extend(
        dirty
            .iter()
            .filter(|path| is_system_markdown(path.as_str()))
            .cloned(),
    );
    let paths = unique_sorted(paths);

    let mut entries = Vec::new();
    for path in paths {
        let state = if dirty.contains(&path) || head.is_none() {
            PalaceEntryState::Uncommitted
        } else {
            PalaceEntryState::Committed
        };
        let Some(raw) = read_entry_text(repo, head, &path, state) else {
            continue;
        };
        let parsed = parse_memory_file(&raw).map_err(|error| PalaceError::Message(error.message))?;
        entries.push(PalaceCoreEntry {
            projection: format!("{MEMORY_DIR_TOKEN}/{path}"),
            path,
            description: parsed.frontmatter.description,
            body: parsed.body,
            state,
        });
    }
    Ok(entries)
}

pub fn collect_external(
    repo: &GitMemoryRepo,
    head: Option<&str>,
) -> Result<Vec<PalaceExternalEntry>, PalaceError> {
    let exec = repo.exec();
    let committed = match head {
        Some(head) => repo
            .ls_tree(Some(head), None)?
            .into_iter()
            .filter(|path| is_external(path.as_str()))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let dirty = dirty_paths(repo);
    let mut paths = committed;
    paths.extend(
        dirty
            .iter()
            .filter(|path| is_external(path.as_str()))
            .cloned(),
    );
    let paths = unique_sorted(paths);

    let mut entries = Vec::new();
    for path in paths {
        let state = if dirty.contains(&path) || head.is_none() {
            PalaceEntryState::Uncommitted
        } else {
            PalaceEntryState::Committed
        };
        let Some(measured) = measure_entry(repo, exec.as_ref(), head, &path, state)? else {
            continue;
        };
        entries.push(PalaceExternalEntry {
            path,
            binary: measured.binary,
            byte_size: measured.byte_size,
            state,
            body: measured.text,
        });
    }
    Ok(entries)
}

fn read_entry_text(
    repo: &GitMemoryRepo,
    head: Option<&str>,
    path: &str,
    state: PalaceEntryState,
) -> Option<String> {
    if state == PalaceEntryState::Uncommitted {
        return std::fs::read_to_string(repo.dir.join(path)).ok();
    }
    let head = head?;
    repo.show(head, path).ok()
}

fn measure_entry(
    repo: &GitMemoryRepo,
    exec: &dyn GitExec,
    head: Option<&str>,
    path: &str,
    state: PalaceEntryState,
) -> Result<Option<Measured>, PalaceError> {
    if state == PalaceEntryState::Uncommitted {
        let Ok(info) = std::fs::metadata(repo.dir.join(path)) else {
            return Ok(None);
        };
        if !info.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(repo.dir.join(path)).ok();
        return Ok(Some(decide(info.len(), text)));
    }
    let Some(head) = head else {
        return Ok(None);
    };
    let Some(size) = blob_size(repo, exec, &format!("{head}:{path}"))? else {
        return Ok(None);
    };
    let text = repo.show(head, path).ok();
    Ok(Some(decide(size, text)))
}

fn blob_size(
    repo: &GitMemoryRepo,
    exec: &dyn GitExec,
    revision: &str,
) -> Result<Option<u64>, PalaceError> {
    let argv = vec![
        "cat-file".to_string(),
        "-s".to_string(),
        revision.to_string(),
    ];
    let options = GitExecOptions {
        cwd: repo.dir.clone(),
        timeout_ms: GIT_TIMEOUT_MS,
        env: BTreeMap::from([("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())]),
        ..GitExecOptions::default()
    };
    let Ok(result) = exec.run(&argv, &options) else {
        return Ok(None);
    };
    if result.code != 0 {
        return Ok(None);
    }
    Ok(result.stdout.trim().parse::<u64>().ok())
}

fn decide(byte_size: u64, text: Option<String>) -> Measured {
    let binary = match text.as_deref() {
        None => true,
        Some(value) => is_binary_text(value) || value.len() as u64 != byte_size,
    };
    if binary {
        Measured {
            binary: true,
            byte_size,
            text: None,
        }
    } else {
        Measured {
            binary: false,
            byte_size,
            text,
        }
    }
}

fn dirty_paths(repo: &GitMemoryRepo) -> BTreeSet<String> {
    let porcelain = repo.status(&[] as &[&str]).unwrap_or_default();
    let mut paths = BTreeSet::new();
    for line in porcelain.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let path = line.chars().skip(3).collect::<String>().trim().to_string();
        let path = path
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .map(str::to_string)
            .unwrap_or(path);
        if !path.is_empty() {
            paths.insert(path);
        }
    }
    paths
}

fn is_system_markdown(path: &str) -> bool {
    path.starts_with(SYSTEM_PREFIX) && path.ends_with(".md")
}

fn is_external(path: &str) -> bool {
    !path.starts_with(SYSTEM_PREFIX) && !path.starts_with(SKILLS_PREFIX)
}

fn is_binary_text(text: &str) -> bool {
    let probe: String = text.chars().take(8_000).collect();
    probe.contains('\u{0}') || probe.contains('\u{FFFD}')
}

fn unique_sorted(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().collect::<BTreeSet<String>>().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::test_support::create_palace_fixture;

    #[test]
    fn committed_core_entries_carry_projection_and_body() {
        let fixture = create_palace_fixture(false);

        let core = collect_core(&fixture.repo, Some(fixture.head.as_str())).unwrap();

        let persona = core.iter().find(|entry| entry.path == "system/persona.md").unwrap();
        assert_eq!(persona.projection, "$MEMORY_DIR/system/persona.md");
        assert!(persona.body.contains("fixture persona"));
        assert_eq!(persona.description, "who the agent is");
        assert!(core.iter().all(|entry| entry.path.starts_with("system/")));
    }

    #[test]
    fn committed_binaries_report_size_only() {
        let fixture = create_palace_fixture(false);

        let external = collect_external(&fixture.repo, Some(fixture.head.as_str())).unwrap();

        let binary = external.iter().find(|entry| entry.path == "reference/logo.png").unwrap();
        assert!(binary.binary);
        assert!(binary.byte_size > 0);
        assert!(binary.body.is_none());
        let text = external.iter().find(|entry| entry.path == "reference/notes.md").unwrap();
        assert!(!text.binary);
        assert!(text.body.as_deref().unwrap_or_default().contains("external note"));
    }

    #[test]
    fn dirty_working_tree_labels_uncommitted() {
        let fixture = create_palace_fixture(false);
        fixture.write_working_file(
            "system/draft.md",
            "---\ndescription: draft\n---\n\nnot committed yet\n",
        );

        let core = collect_core(&fixture.repo, Some(fixture.head.as_str())).unwrap();

        let draft = core.iter().find(|entry| entry.path == "system/draft.md").unwrap();
        assert_eq!(draft.state, PalaceEntryState::Uncommitted);
        assert_eq!(draft.state.label(), UNCOMMITTED_LABEL);
        let persona = core.iter().find(|entry| entry.path == "system/persona.md").unwrap();
        assert_eq!(persona.state, PalaceEntryState::Committed);
    }
}
