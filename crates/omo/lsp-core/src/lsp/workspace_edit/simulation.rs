//! Port of TS `workspace-edit-simulation.ts`.

use super::text::normalize_text_edits;
use super::types::EntryKind;
use super::types::ParseFailure;
use super::types::ParsedWorkspaceOperation as Parsed;
use super::types::PlannedWorkspaceOperation as Planned;
use super::types::WorkspaceEditValidationError as VErr;
use super::types::WorkspaceSnapshotEntry as Entry;
use crate::request_context::dirname;
use indexmap::IndexMap;
use std::path::Path;

type Virtual = IndexMap<String, Entry>;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SimulatedWorkspaceEdit {
    pub operations: Vec<Planned>,
    pub failures: Vec<ParseFailure>,
}

fn is_same_or_descendant(candidate: &str, parent: &str) -> bool {
    Path::new(candidate).starts_with(parent)
}

fn remove_subtree(virtual_fs: &mut Virtual, path: &str) {
    virtual_fs.retain(|candidate, _| !is_same_or_descendant(candidate, path));
    virtual_fs.insert(path.to_string(), Entry::Missing);
}

fn move_subtree(virtual_fs: &mut Virtual, old_path: &str, new_path: &str) {
    let moved: Vec<(String, Entry)> = virtual_fs
        .iter()
        .filter(|(candidate, _)| is_same_or_descendant(candidate, old_path))
        .map(|(candidate, entry)| (candidate.clone(), entry.clone()))
        .collect();
    remove_subtree(virtual_fs, old_path);
    remove_subtree(virtual_fs, new_path);
    for (candidate, entry) in moved {
        let suffix = Path::new(&candidate)
            .strip_prefix(old_path)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let target = if suffix.as_os_str().is_empty() {
            new_path.to_string()
        } else {
            Path::new(new_path)
                .join(suffix)
                .to_string_lossy()
                .into_owned()
        };
        virtual_fs.insert(target, entry);
    }
}

fn has_children(virtual_fs: &Virtual, path: &str) -> bool {
    virtual_fs.iter().any(|(candidate, entry)| {
        candidate != path && !entry.is_missing() && is_same_or_descendant(candidate, path)
    })
}

fn require_parent(virtual_fs: &Virtual, path: &str, change_index: usize) -> Result<(), VErr> {
    match virtual_fs.get(&dirname(path)) {
        Some(Entry::Directory { .. }) => Ok(()),
        _ => Err(VErr::new(
            change_index,
            format!("parent directory does not exist for {path}"),
        )),
    }
}

fn reject_symlink(followed: bool, change_index: usize) -> Result<(), VErr> {
    if followed {
        return Err(VErr::new(
            change_index,
            "resource operations through symbolic links are unsupported",
        ));
    }
    Ok(())
}

/// TS `simulateOperations`: validate the declared sequence against a virtual copy.
pub fn simulate_operations(
    parsed: &[Parsed],
    snapshots: &IndexMap<String, Entry>,
) -> SimulatedWorkspaceEdit {
    let mut virtual_fs = snapshots.clone();
    let mut result = SimulatedWorkspaceEdit::default();
    for operation in parsed {
        match simulate(operation, &mut virtual_fs) {
            Ok(planned) => result.operations.push(planned),
            Err(error) => result.failures.push(ParseFailure {
                change_index: operation.change_index(),
                message: error.detail,
            }),
        }
    }
    result
}

fn entry_of(virtual_fs: &Virtual, path: &str) -> Entry {
    virtual_fs.get(path).cloned().unwrap_or(Entry::Missing)
}

fn simulate(operation: &Parsed, virtual_fs: &mut Virtual) -> Result<Planned, VErr> {
    match operation {
        Parsed::Text {
            change_index,
            path,
            edits,
            version,
            ..
        } => {
            let Some(Entry::File { content }) = virtual_fs.get(path).cloned() else {
                return Err(VErr::new(*change_index, format!("{path} is not a file")));
            };
            let normalized = normalize_text_edits(&content, edits, *change_index)?;
            virtual_fs.insert(
                path.clone(),
                Entry::File {
                    content: normalized.text.clone(),
                },
            );
            Ok(Planned::Text {
                change_index: *change_index,
                path: path.clone(),
                before_text: content,
                after_text: normalized.text,
                edit_count: normalized.edits.len(),
                document_version: *version,
            })
        }
        Parsed::Create {
            change_index,
            path,
            overwrite,
            ignore_if_exists,
            followed_symbolic_link,
            ..
        } => {
            let change_index = *change_index;
            reject_symlink(*followed_symbolic_link, change_index)?;
            require_parent(virtual_fs, path, change_index)?;
            let target = entry_of(virtual_fs, path);
            let empty = Entry::File {
                content: String::new(),
            };
            if !target.is_missing() {
                if *overwrite && matches!(target, Entry::File { .. }) {
                    virtual_fs.insert(path.clone(), empty);
                    return Ok(Planned::Create {
                        change_index,
                        path: path.clone(),
                        replaced: true,
                    });
                }
                if *ignore_if_exists {
                    return Ok(Planned::Noop { change_index });
                }
                return Err(VErr::new(
                    change_index,
                    format!("create target already exists: {path}"),
                ));
            }
            virtual_fs.insert(path.clone(), empty);
            Ok(Planned::Create {
                change_index,
                path: path.clone(),
                replaced: false,
            })
        }
        Parsed::Rename {
            change_index,
            old_path,
            new_path,
            overwrite,
            ignore_if_exists,
            followed_symbolic_link,
            ..
        } => {
            let change_index = *change_index;
            reject_symlink(*followed_symbolic_link, change_index)?;
            let Some(source_kind) = entry_of(virtual_fs, old_path).entry_kind() else {
                return Err(VErr::new(
                    change_index,
                    format!("rename source does not exist: {old_path}"),
                ));
            };
            if old_path == new_path {
                return Ok(Planned::Noop { change_index });
            }
            if is_same_or_descendant(new_path, old_path) {
                return Err(VErr::new(
                    change_index,
                    "cannot rename a path into its own subtree",
                ));
            }
            require_parent(virtual_fs, new_path, change_index)?;
            let destination_exists = !entry_of(virtual_fs, new_path).is_missing();
            if destination_exists && !*overwrite {
                if *ignore_if_exists {
                    return Ok(Planned::Noop { change_index });
                }
                return Err(VErr::new(
                    change_index,
                    format!("rename target already exists: {new_path}"),
                ));
            }
            move_subtree(virtual_fs, old_path, new_path);
            Ok(Planned::Rename {
                change_index,
                old_path: old_path.clone(),
                new_path: new_path.clone(),
                source_kind,
                replace_destination: destination_exists,
            })
        }
        Parsed::Delete {
            change_index,
            path,
            recursive,
            ignore_if_not_exists,
            followed_symbolic_link,
            ..
        } => {
            let change_index = *change_index;
            reject_symlink(*followed_symbolic_link, change_index)?;
            let Some(target_kind) = entry_of(virtual_fs, path).entry_kind() else {
                if *ignore_if_not_exists {
                    return Ok(Planned::Noop { change_index });
                }
                return Err(VErr::new(
                    change_index,
                    format!("delete target does not exist: {path}"),
                ));
            };
            if target_kind == EntryKind::Directory && !*recursive && has_children(virtual_fs, path)
            {
                return Err(VErr::new(
                    change_index,
                    format!("directory is not empty: {path}"),
                ));
            }
            remove_subtree(virtual_fs, path);
            Ok(Planned::Delete {
                change_index,
                path: path.clone(),
                target_kind,
                recursive: *recursive,
            })
        }
    }
}
