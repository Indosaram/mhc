//! Port of `permission-system/external-pattern.ts` (senpi v2026.10.27-2, senpi#4351).
//!
//! `external_directory` rules also match by filesystem identity: the rule's literal prefix and the
//! value's ancestors are compared by device/inode, so a symlinked or case-variant spelling of a
//! ruled directory is still governed by that rule. Identity uses `stat`/`lstat` only, never open(2).
//!
//! PERF-DIVERGENCE(senpi#4351): none; DIVERGENCE: on non-Unix targets std exposes no stable
//! device/inode pair, so identity lookups fail and only the lexical wildcard match applies there.

use crate::wildcard;
use std::{
    cell::RefCell,
    collections::HashMap,
    io,
    path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path},
};

pub(crate) type Identity = (u64, u64);

/// `stat` identity (follows links), like `statSync(path, { bigint: true })`.
pub(crate) fn stat_identity(path: &str) -> io::Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path)?;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no file identity on this platform",
        ))
    }
}

/// `lstat` identity (does not follow a final link).
pub(crate) fn lstat_identity(path: &str) -> io::Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        std::fs::symlink_metadata(path)?;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no file identity on this platform",
        ))
    }
}

pub(crate) fn is_missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

pub(crate) fn is_absolute(value: &str) -> bool {
    Path::new(value).is_absolute()
}

/// `path.parse(value).root`.
pub(crate) fn root_of(value: &str) -> String {
    let path = Path::new(value);
    let mut root = String::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => {
                root.push_str(&prefix.as_os_str().to_string_lossy())
            }
            std::path::Component::RootDir => root.push(MAIN_SEPARATOR),
            _ => break,
        }
    }
    root
}

pub(crate) fn join_sep(parent: &str, child: &str) -> String {
    if parent.ends_with(MAIN_SEPARATOR) {
        format!("{parent}{child}")
    } else {
        format!("{parent}{MAIN_SEPARATOR}{child}")
    }
}

/// `path.dirname`: the root is its own parent.
pub(crate) fn dirname(value: &str) -> String {
    Path::new(value).parent().map_or_else(
        || value.to_owned(),
        |parent| parent.to_string_lossy().into_owned(),
    )
}

/// `path.basename`.
pub(crate) fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn list_directory(directory: &str) -> io::Result<Vec<String>> {
    std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect()
}

/// Recover on-disk case without replacing any lexical ancestor with its symlink target.
pub fn canonical_path_case(value: &str) -> String {
    canonical_path_case_with(value, &mut list_directory)
}

pub(crate) fn canonical_path_case_with(
    value: &str,
    list: &mut dyn FnMut(&str) -> io::Result<Vec<String>>,
) -> String {
    if !is_absolute(value) {
        return value.to_owned();
    }
    let root = root_of(value);
    let mut parts: Vec<String> = value[root.len()..]
        .split(MAIN_SEPARATOR)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let mut parent = root;
    for index in 0..parts.len() {
        let attempt = (|| -> io::Result<()> {
            let entries = list(&parent)?;
            if !entries.contains(&parts[index]) {
                let identity = lstat_identity(&join_sep(&parent, &parts[index]))?;
                let mut spelling = None;
                for entry in &entries {
                    if entry.to_lowercase() != parts[index].to_lowercase() {
                        continue;
                    }
                    if lstat_identity(&join_sep(&parent, entry))? == identity {
                        spelling = Some(entry.clone());
                        break;
                    }
                }
                if let Some(spelling) = spelling {
                    parts[index] = spelling;
                }
            }
            Ok(())
        })();
        if attempt.is_err() {
            // Missing or unreadable components have no proven alternative spelling.
            return join_sep(&parent, &parts[index..].join(MAIN_SEPARATOR_STR));
        }
        parent = join_sep(&parent, &parts[index]);
    }
    parent
}

/// Case aliases must name the same component, not merely symlinks to the same destination.
pub fn same_lexical_path(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    if left.to_lowercase() != right.to_lowercase() {
        return false;
    }
    let (left_root, right_root) = (root_of(left), root_of(right));
    let left_parts: Vec<&str> = left[left_root.len()..].split(MAIN_SEPARATOR).collect();
    let right_parts: Vec<&str> = right[right_root.len()..].split(MAIN_SEPARATOR).collect();
    for index in 0..left_parts.len() {
        if left_parts.get(index) == right_parts.get(index) {
            continue;
        }
        let Some(right_prefix) = right_parts.get(..=index) else {
            return false;
        };
        let a = lstat_identity(&format!(
            "{left_root}{}",
            left_parts[..=index].join(MAIN_SEPARATOR_STR)
        ));
        let b = lstat_identity(&format!(
            "{right_root}{}",
            right_prefix.join(MAIN_SEPARATOR_STR)
        ));
        match (a, b) {
            (Ok(a), Ok(b)) if a == b => {}
            _ => return false,
        }
    }
    true
}

/// A matcher that keeps filesystem observations within one evaluation, never across decisions.
pub struct ExternalPathMatcher<'a> {
    preserve_ancestry: bool,
    canonical_case: Option<&'a dyn Fn(&str) -> String>,
    resolved: RefCell<HashMap<String, String>>,
    identities: RefCell<HashMap<String, Result<Identity, io::ErrorKind>>>,
}

pub fn create_external_path_matcher<'a>(
    preserve_ancestry: bool,
    canonical_case: Option<&'a dyn Fn(&str) -> String>,
) -> ExternalPathMatcher<'a> {
    ExternalPathMatcher {
        preserve_ancestry,
        canonical_case,
        resolved: RefCell::default(),
        identities: RefCell::default(),
    }
}

impl ExternalPathMatcher<'_> {
    fn realpath(&self, value: &str) -> String {
        if let Some(result) = self.resolved.borrow().get(value) {
            return result.clone();
        }
        let result = if self.preserve_ancestry {
            self.canonical_case
                .map_or_else(|| canonical_path_case(value), |canonical| canonical(value))
        } else {
            crate::external_dir::resolve_without_open(Path::new(value))
                .to_string_lossy()
                .into_owned()
        };
        self.resolved
            .borrow_mut()
            .insert(value.to_owned(), result.clone());
        result
    }

    fn stat(&self, value: &str) -> Result<Identity, io::ErrorKind> {
        if let Some(result) = self.identities.borrow().get(value) {
            return *result;
        }
        let result = stat_identity(value).map_err(|error| {
            if is_missing(&error) {
                io::ErrorKind::NotFound
            } else {
                error.kind()
            }
        });
        self.identities
            .borrow_mut()
            .insert(value.to_owned(), result);
        result
    }

    /// Match the literal prefix and glob components by filesystem identity. This also covers
    /// remembered scopes and missing descendants. Lexical matching keeps ancestry and
    /// canonicalizes case through directory entries; resolved matching follows literal prefixes.
    pub fn matches(&self, value: &str, pattern: &str) -> bool {
        if !is_absolute(value) || !is_absolute(pattern) {
            return false;
        }
        let wildcard = pattern.find(['*', '?']);
        let (mut prefix, mut suffix): (String, Vec<String>) = match wildcard {
            None => (pattern.to_owned(), Vec::new()),
            Some(wildcard) => {
                let head = &pattern[..wildcard];
                let boundary = head
                    .rfind('/')
                    .into_iter()
                    .chain(head.rfind(MAIN_SEPARATOR))
                    .max()
                    .unwrap_or(0);
                (
                    pattern[..=boundary].to_owned(),
                    vec![pattern[boundary + 1..].to_owned()],
                )
            }
        };
        prefix = self.realpath(&prefix);
        let identity = loop {
            match self.stat(&prefix) {
                Ok(identity) => break identity,
                Err(io::ErrorKind::NotFound) => {
                    let parent = dirname(&prefix);
                    if parent == prefix {
                        return false;
                    }
                    suffix.insert(0, basename(&prefix));
                    prefix = parent;
                }
                Err(_) => return false,
            }
        };
        let mut ancestor = self.realpath(value);
        let mut tail: Vec<String> = Vec::new();
        loop {
            match self.stat(&ancestor) {
                Ok(candidate)
                    if candidate == identity
                        && (!self.preserve_ancestry || same_lexical_path(&ancestor, &prefix)) =>
                {
                    if tail.is_empty() && !suffix.is_empty() {
                        return false;
                    }
                    let suffix = suffix.join("/").replace(MAIN_SEPARATOR, "/");
                    return matches_suffix(&ancestor, &tail, &suffix);
                }
                Ok(_) | Err(io::ErrorKind::NotFound) => {}
                Err(_) => return false,
            }
            let parent = dirname(&ancestor);
            if parent == ancestor {
                return false;
            }
            tail.insert(0, basename(&ancestor));
            ancestor = parent;
        }
    }
}

fn matches_component(parent: &str, value: &str, pattern: &str) -> bool {
    if wildcard::matches(value, pattern) {
        return true;
    }
    if !wildcard::matches(&value.to_lowercase(), &pattern.to_lowercase()) {
        return false;
    }
    // Instantiate the rule's literal spelling, retaining what each wildcard consumed. Case-
    // insensitive text matching only proposes an alias: lexical entry identity must prove it.
    let value_chars: Vec<char> = value.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let mut spelling = value_chars.clone();
    let (mut value_index, mut pattern_index) = (0, 0);
    let mut star: Option<usize> = None;
    let mut consumed = 0;
    let lower = |ch: char| ch.to_lowercase().collect::<String>();
    while value_index < value_chars.len() {
        let token = pattern_chars.get(pattern_index).copied();
        if token == Some('*') {
            star = Some(pattern_index);
            pattern_index += 1;
            consumed = value_index;
        } else if let Some(token) =
            token.filter(|token| *token == '?' || lower(*token) == lower(value_chars[value_index]))
        {
            spelling[value_index] = if token == '?' {
                value_chars[value_index]
            } else {
                token
            };
            value_index += 1;
            pattern_index += 1;
        } else if let Some(star) = star {
            // Backtracking into * discards the literal casing from its previous attempt.
            spelling[consumed..value_index].copy_from_slice(&value_chars[consumed..value_index]);
            consumed += 1;
            value_index = consumed;
            pattern_index = star + 1;
        } else {
            return false;
        }
    }
    while pattern_chars.get(pattern_index) == Some(&'*') {
        pattern_index += 1;
    }
    if pattern_index != pattern_chars.len() {
        return false;
    }
    let spelling: String = spelling.into_iter().collect();
    same_lexical_path(&join_sep(parent, value), &join_sep(parent, &spelling))
}

fn matches_suffix(parent: &str, parts: &[String], pattern: &str) -> bool {
    if wildcard::matches(&parts.join("/"), pattern) {
        return true;
    }
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let slice = |start: usize, end: usize| pattern_chars[start..end].iter().collect::<String>();
    let mut parent = parent.to_owned();
    let mut positions = std::collections::BTreeSet::from([0usize]);
    for (index, value) in parts.iter().enumerate() {
        let mut next = std::collections::BTreeSet::new();
        for &start in &positions {
            if index == parts.len() - 1 {
                if matches_component(&parent, value, &slice(start, pattern_chars.len())) {
                    return true;
                }
                continue;
            }
            // Wildcard matching lets both * and ? consume '/'. Keep every possible pattern
            // position at a component boundary, rather than choosing one greedy match whose case
            // spelling could fail on a different volume.
            for end in start..pattern_chars.len() {
                let token = pattern_chars[end];
                if token != '*' && token != '?' && token != '/' {
                    continue;
                }
                let component = slice(start, if token == '*' { end + 1 } else { end });
                if matches_component(&parent, value, &component) {
                    next.insert(if token == '*' { end } else { end + 1 });
                }
                if token == '/' {
                    break;
                }
            }
        }
        if next.is_empty() {
            return false;
        }
        positions = next;
        parent = join_sep(&parent, value);
    }
    false
}
