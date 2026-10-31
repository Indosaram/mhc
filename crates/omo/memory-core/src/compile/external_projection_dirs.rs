//! Groups external memory paths per directory, with redaction-safe labels and pointers.

use std::collections::{HashMap, HashSet};

use crate::sync::redact::redact_secret_like_material;

use super::external_projection_names::{NamedFile, generated_ids, locale_compare};

const MEMORY_DIR: &str = "$MEMORY_DIR";
const MIN_SIBLINGS: usize = 3;

/// One listed line: a directory, or sibling directories that list as one.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryGroup {
    pub label: String,
    /// Where the hidden names of this line can be listed.
    pub pointer: String,
    pub files: Vec<NamedFile>,
    /// Several sibling directories list as this one line.
    pub grouped: bool,
}

struct SiblingGroup {
    members: Vec<String>,
    line: DirectoryGroup,
}

/// Groups files by directory. Leaf sibling directories named by generated ids list as one
/// `<parent>/<id>/ (N dirs)` line; other leaf siblings holding exactly the same file names list as
/// one `<parent>/{a, b, c}/` line. Lines keep name order, the repository root first.
pub fn group_directories(
    paths: &[String],
    times: Option<&std::collections::BTreeMap<String, i64>>,
) -> Vec<DirectoryGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut by_directory: HashMap<String, Vec<NamedFile>> = HashMap::new();
    for path in paths {
        let mut parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        let Some(name) = parts.pop() else {
            continue;
        };
        let directory = if parts.is_empty() {
            String::new()
        } else {
            format!("{}/", parts.join("/"))
        };
        let file = NamedFile {
            name: name.to_string(),
            time: times
                .and_then(|times| times.get(path))
                .map_or(f64::NEG_INFINITY, |time| *time as f64),
            files: 1,
        };
        match by_directory.get_mut(&directory) {
            Some(files) => files.push(file),
            None => {
                order.push(directory.clone());
                by_directory.insert(directory, vec![file]);
            }
        }
    }
    let leaves = leaf_directories(&order);
    let mut groups = id_families(&by_directory, &leaves);
    groups.extend(uniform_siblings(&by_directory, &leaves));
    let grouped: HashSet<&String> = groups.iter().flat_map(|group| &group.members).collect();
    let mut lines: Vec<DirectoryGroup> = order
        .iter()
        .filter(|directory| !grouped.contains(directory))
        .map(|directory| {
            let (label, pointer) = labelled(directory);
            DirectoryGroup {
                label,
                pointer,
                files: by_directory[directory].clone(),
                grouped: false,
            }
        })
        .collect();
    lines.extend(groups.into_iter().map(|group| group.line));
    let root_label = format!("{MEMORY_DIR}/");
    let (mut sorted, rest): (Vec<_>, Vec<_>) =
        lines.into_iter().partition(|line| line.label == root_label);
    let mut rest = rest;
    rest.sort_by(|a, b| locale_compare(&a.label, &b.label));
    sorted.extend(rest);
    sorted
}

fn labelled(directory: &str) -> (String, String) {
    if directory.is_empty() {
        return (format!("{MEMORY_DIR}/"), format!("{MEMORY_DIR}/"));
    }
    let parts: Vec<&str> = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let readable = &parts[..first_redacted(&parts)];
    let label = format!(
        "{}/",
        parts
            .iter()
            .map(|part| redact_secret_like_material(part))
            .collect::<Vec<_>>()
            .join("/")
    );
    let pointer = if readable.is_empty() {
        format!("{MEMORY_DIR}/")
    } else {
        format!("{MEMORY_DIR}/{}/", readable.join("/"))
    };
    (label, pointer)
}

fn first_redacted(parts: &[&str]) -> usize {
    parts
        .iter()
        .position(|part| redact_secret_like_material(part) != *part)
        .unwrap_or(parts.len())
}

/// Insertion-ordered leaf siblings keyed by parent.
type Leaves = Vec<(String, Vec<String>)>;

/// Directories with no directory below them, keyed by parent: only those can share a line.
fn leaf_directories(directories: &[String]) -> Leaves {
    let mut leaves: Leaves = Vec::new();
    for directory in directories {
        if directory.is_empty()
            || directories
                .iter()
                .any(|other| other != directory && other.starts_with(directory.as_str()))
        {
            continue;
        }
        let trimmed = &directory[..directory.len() - 1];
        let parent = &directory[..trimmed.rfind('/').map_or(0, |at| at + 1)];
        let name = segment(directory, parent);
        if redact_secret_like_material(name) != name {
            continue;
        }
        match leaves.iter_mut().find(|(key, _)| key == parent) {
            Some((_, siblings)) => siblings.push(directory.clone()),
            None => leaves.push((parent.to_string(), vec![directory.clone()])),
        }
    }
    leaves
}

fn segment<'a>(directory: &'a str, parent: &str) -> &'a str {
    &directory[parent.len()..directory.len() - 1]
}

fn files_of(by_directory: &HashMap<String, Vec<NamedFile>>, members: &[String]) -> Vec<NamedFile> {
    members
        .iter()
        .flat_map(|directory| by_directory.get(directory).cloned().unwrap_or_default())
        .collect()
}

fn parent_label(parent: &str) -> (String, String) {
    let (label, pointer) = labelled(parent);
    (
        if parent.is_empty() {
            String::new()
        } else {
            label
        },
        pointer,
    )
}

fn id_families(
    by_directory: &HashMap<String, Vec<NamedFile>>,
    leaves: &Leaves,
) -> Vec<SiblingGroup> {
    let mut groups = Vec::new();
    for (parent, siblings) in leaves {
        let names: Vec<&str> = siblings
            .iter()
            .map(|directory| segment(directory, parent))
            .collect();
        let ids = generated_ids(&names);
        let members: Vec<String> = siblings
            .iter()
            .filter(|directory| ids.contains(segment(directory, parent)))
            .cloned()
            .collect();
        if members.len() < MIN_SIBLINGS {
            continue;
        }
        let (label, pointer) = parent_label(parent);
        groups.push(SiblingGroup {
            line: DirectoryGroup {
                label: format!("{label}<id>/ ({} dirs)", members.len()),
                pointer,
                files: files_of(by_directory, &members),
                grouped: true,
            },
            members,
        });
    }
    groups
}

fn uniform_siblings(
    by_directory: &HashMap<String, Vec<NamedFile>>,
    leaves: &Leaves,
) -> Vec<SiblingGroup> {
    let mut groups = Vec::new();
    for (parent, siblings) in leaves {
        let names: Vec<&str> = siblings
            .iter()
            .map(|directory| segment(directory, parent))
            .collect();
        let ids = generated_ids(&names);
        let mut by_names: Vec<(String, Vec<String>)> = Vec::new();
        for directory in siblings {
            if ids.contains(segment(directory, parent)) {
                continue;
            }
            let mut file_names: Vec<&str> = by_directory
                .get(directory)
                .map(|files| files.iter().map(|file| file.name.as_str()).collect())
                .unwrap_or_default();
            file_names.sort_unstable();
            let key = file_names.join("\0");
            match by_names.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, same)) => same.push(directory.clone()),
                None => by_names.push((key, vec![directory.clone()])),
            }
        }
        for (_, members) in by_names {
            if members.len() < MIN_SIBLINGS
                || members.iter().any(|directory| {
                    let name = segment(directory, parent);
                    name.contains(['{', '}']) || name.contains(", ")
                })
            {
                continue;
            }
            let (label, pointer) = parent_label(parent);
            let mut names: Vec<&str> = members
                .iter()
                .map(|directory| segment(directory, parent))
                .collect();
            names.sort_by(|a, b| locale_compare(a, b));
            groups.push(SiblingGroup {
                line: DirectoryGroup {
                    label: format!("{label}{{{}}}/", names.join(", ")),
                    pointer,
                    files: files_of(by_directory, &members),
                    grouped: true,
                },
                members,
            });
        }
    }
    groups
}

#[cfg(test)]
#[path = "external_projection_dirs_tests.rs"]
mod tests;
