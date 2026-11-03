//! Port of `permission-system/external-spellings.ts` (senpi v2026.10.27-2, senpi#4351).

use crate::{
    external_pattern::{
        canonical_path_case_with, is_absolute, join_sep, root_of, same_lexical_path,
    },
    types::{Action, Rule},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    io,
    path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path},
};

/// One decision and the rules that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub rules: Vec<Rule>,
}
impl Decision {
    pub(crate) fn ask() -> Self {
        Self {
            action: Action::Ask,
            rules: Vec::new(),
        }
    }
}

const MAX_SPELLINGS: usize = 16;
const MAX_HOPS: usize = 40;

fn absolute(value: &str) -> String {
    if is_absolute(value) {
        return value.to_owned();
    }
    let cwd = std::env::current_dir()
        .map(|cwd| cwd.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{cwd}{MAIN_SEPARATOR}{value}")
}

/// `path.join(physical, part)` for one plain component.
fn join_component(parent: &str, part: &str) -> String {
    join_sep(parent, part)
}

/// `path.dirname` of a physical position.
fn physical_parent(physical: &str) -> String {
    crate::external_pattern::dirname(physical)
}

type PathDecision<'a> = dyn FnMut(&str, &str, &dyn Fn(&str) -> String) -> Decision + 'a;

/// Walk a component queue like the kernel: the position is always physical, and link targets are
/// prepended without normalizing away dots. A `..` moves to the physical parent's directory. Keep
/// the absolute request, the physical prefix plus unresolved suffix at each followed link, and the
/// final physical path, with proven case forms. Never combine a relative target with an unresolved
/// lexical parent. Discovery uses only lstat/readlink; case recovery is separate.
pub fn decide_external_paths(
    targets: &[&str],
    aliases: &[String],
    decide: &mut PathDecision<'_>,
) -> Decision {
    let directories: RefCell<HashMap<String, Vec<String>>> = RefCell::default();
    let cases: RefCell<HashMap<String, String>> = RefCell::default();
    let canonical_case = |value: &str| -> String {
        if let Some(result) = cases.borrow().get(value) {
            return result.clone();
        }
        let result = canonical_path_case_with(value, &mut |directory: &str| {
            if let Some(entries) = directories.borrow().get(directory) {
                return Ok(entries.clone());
            }
            let entries: Vec<String> = std::fs::read_dir(directory)?
                .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
                .collect::<io::Result<_>>()?;
            directories
                .borrow_mut()
                .insert(directory.to_owned(), entries.clone());
            Ok(entries)
        });
        cases.borrow_mut().insert(value.to_owned(), result.clone());
        result
    };
    let mut resolved_paths: HashMap<String, String> = HashMap::new();
    let mut resolve = |value: &str| -> io::Result<String> {
        if let Some(result) = resolved_paths.get(value) {
            return Ok(result.clone());
        }
        let result =
            maho_tools::path_utils::realpath_without_open_strict(Path::new(&absolute(value)))?
                .to_string_lossy()
                .into_owned();
        resolved_paths.insert(value.to_owned(), result.clone());
        Ok(result)
    };
    // Insertion-ordered like the TS `Map`.
    let mut spellings: Vec<(String, String)> = Vec::new();
    let add = |spellings: &mut Vec<(String, String)>, spelling: String, resolved: &str| -> bool {
        for candidate in [spelling.clone(), canonical_case(&spelling)] {
            if let Some(idx) = spellings.iter().position(|(known, _)| *known == candidate) {
                spellings[idx].1 = resolved.to_owned();
            } else if spellings.len() >= MAX_SPELLINGS {
                return false;
            } else {
                spellings.push((candidate, resolved.to_owned()));
            }
        }
        true
    };
    let mut hops = 0;
    let walked = (|| -> io::Result<bool> {
        let resolved_targets: Vec<String> = targets
            .iter()
            .map(|target| resolve(target))
            .collect::<io::Result<_>>()?;
        let mut values: Vec<&str> = Vec::new();
        for value in targets
            .iter()
            .copied()
            .chain(aliases.iter().map(String::as_str))
        {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        for value in values {
            let resolved = resolve(value)?;
            // A multi-path request's aliases belong only to their own resolved target.
            if !resolved_targets
                .iter()
                .any(|candidate| same_lexical_path(candidate, &resolved))
            {
                continue;
            }
            if !add(&mut spellings, resolved.clone(), &resolved) {
                return Ok(false);
            }
            let spelling = absolute(value);
            if !add(&mut spellings, spelling.clone(), &resolved) {
                return Ok(false);
            }
            let mut physical = root_of(&spelling);
            let mut pending: std::collections::VecDeque<String> = spelling[physical.len()..]
                .split(MAIN_SEPARATOR)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect();
            while let Some(part) = pending.pop_front() {
                if part == "." {
                    continue;
                }
                if part == ".." {
                    physical = physical_parent(&physical);
                    continue;
                }
                let candidate = join_component(&physical, &part);
                let link = match std::fs::symlink_metadata(&candidate) {
                    Ok(meta) if !meta.file_type().is_symlink() => {
                        physical = candidate;
                        continue;
                    }
                    Ok(_) => {
                        hops += 1;
                        if hops > MAX_HOPS {
                            return Ok(false);
                        }
                        // Concatenate the unresolved suffix verbatim: a join would collapse `S/..`
                        // before S was followed and could erase a genuine intermediate deny.
                        let intermediate = std::iter::once(candidate.clone())
                            .chain(pending.iter().cloned())
                            .collect::<Vec<_>>()
                            .join(MAIN_SEPARATOR_STR);
                        if !add(&mut spellings, intermediate, &resolved) {
                            return Ok(false);
                        }
                        std::fs::read_link(&candidate)?
                            .to_string_lossy()
                            .into_owned()
                    }
                    Err(error) if crate::external_pattern::is_missing(&error) => {
                        // No links can be followed inside a missing tail.
                        let folded = pending
                            .iter()
                            .fold(candidate, |path, part| join_component(&path, part));
                        physical = crate::external_dir::lexical_resolve(
                            Path::new(&root_of(&folded)),
                            Path::new(&folded),
                        )
                        .to_string_lossy()
                        .into_owned();
                        break;
                    }
                    Err(error) => return Err(error),
                };
                let parts: Vec<String> = if is_absolute(&link) {
                    physical = root_of(&link);
                    link[physical.len()..]
                        .split(MAIN_SEPARATOR)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect()
                } else {
                    link.split(MAIN_SEPARATOR)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect()
                };
                for part in parts.into_iter().rev() {
                    pending.push_front(part);
                }
            }
            if !add(&mut spellings, physical, &resolved) {
                return Ok(false);
            }
        }
        Ok(true)
    })();
    // Missing tails are supported by the strict resolver; loops, hop exhaustion and unavailable
    // metadata cannot establish the complete spelling set.
    if !matches!(walked, Ok(true)) {
        return Decision::ask();
    }
    let mut action = Action::Allow;
    let mut rules = Vec::new();
    for (spelling, resolved) in &spellings {
        let decision = decide(spelling, resolved, &canonical_case);
        if decision.action == Action::Deny
            || (decision.action == Action::Ask && action == Action::Allow)
        {
            action = decision.action;
        }
        rules.extend(decision.rules);
    }
    Decision { action, rules }
}
