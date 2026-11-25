//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-run-grammar.ts`.
//!
//! The `run` git grammar (S40 PR 2, design v2.6, `senpi#5645`): every argument of every admitted
//! subcommand has a declared position and kind, taken from the memory component's call sites. A
//! flag never takes a value unless its shape says so, so a boolean flag cannot swallow the next
//! argument, and a path is only ever a `path`/`worktree` slot the hand confines.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rev,
    Range,
    Revish,
    Object,
    Blobref,
    Ref,
    Refpat,
    Branch,
    Refspec,
    Msg,
    Count,
    Mode,
    Oid,
    Filter,
    Fmt,
    Iso,
    Relpath,
    Gitpath,
    Path,
    Worktree,
    Source,
    Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedPathKind {
    Path,
    Worktree,
    Source,
    Target,
}

/// A path-kind argument the hand still has to confine on the device before git runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedPath {
    /// `Path`: inside the repo; `Worktree`: outside the git dirs (write); `Source`: a local url
    /// read; `Target`: a local url written.
    pub kind: TypedPathKind,
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum Item {
    Literal(&'static str),
    Kind(Kind),
    Prefixed { prefix: &'static str, kind: Kind },
    Optional(Vec<Item>),
    Repeat { items: Vec<Item>, min: usize },
}

fn lit(s: &'static str) -> Item {
    Item::Literal(s)
}
fn k(kind: Kind) -> Item {
    Item::Kind(kind)
}
fn eq(prefix: &'static str, kind: Kind) -> Item {
    Item::Prefixed { prefix, kind }
}
fn opt(items: Vec<Item>) -> Item {
    Item::Optional(items)
}
fn rep(min: usize, items: Vec<Item>) -> Item {
    Item::Repeat { items, min }
}

fn is_rev_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_' | b'/' | b'@' | b'{' | b'}' | b'^' | b'~' | b'+' | b'-'
        )
}

fn is_rev(value: &str) -> bool {
    let Some(first) = value.bytes().next() else {
        return false;
    };
    if matches!(first, b'-' | b'.' | b'/' | b'~' | b':') {
        return false;
    }
    value.bytes().all(is_rev_char) && !value.contains("..") && !value.contains("//")
}

fn is_range(value: &str) -> bool {
    let mut parts = value.split("..");
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => is_rev(a) && is_rev(b),
        _ => false,
    }
}

fn is_rel(value: &str) -> bool {
    let Some(first) = value.bytes().next() else {
        return false;
    };
    if first == b'/' || first == b'-' || value.contains('\0') {
        return false;
    }
    value.split('/').all(|seg| seg != "..")
}

fn is_blobref(value: &str) -> bool {
    let Some(at) = value.find(':') else {
        return false;
    };
    (at == 0 || is_rev(&value[..at])) && is_rel(&value[at + 1..])
}

fn is_filesystem(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0') && !value.starts_with(['-', ':'])
}

fn scheme(value: &str) -> Option<&str> {
    let idx = value.find("://")?;
    let s = &value[..idx];
    let mut bytes = s.bytes();
    let first = bytes.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    bytes
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
        .then_some(s)
}

/// git's `url_is_local_not_ssh`: no colon, a slash before the first colon, or (on Windows only) a
/// DOS drive means local.
fn url_is_local(value: &str) -> bool {
    let colon = value.find(':');
    let slash = value.find('/');
    match (colon, slash) {
        (None, _) => true,
        (Some(c), Some(s)) if s < c => true,
        _ => {
            cfg!(windows) && {
                let b = value.as_bytes();
                b.len() >= 3
                    && b[0].is_ascii_alphabetic()
                    && b[1] == b':'
                    && (b[2] == b'\\' || b[2] == b'/')
            }
        }
    }
}

/// A transport git refuses here: NUL, whitespace, an option-like value, or a `<helper>::<address>`
/// remote helper.
fn unsafe_url(value: &str) -> bool {
    value.is_empty()
        || value.contains('\0')
        || value.chars().any(char::is_whitespace)
        || value.starts_with('-')
        || value.contains("::")
}

/// A url git reaches over the network: ssh, https, or the scp form `host:path`.
fn is_remote_url(value: &str) -> bool {
    if unsafe_url(value) {
        return false;
    }
    if let Some(named) = scheme(value) {
        return named == "https" || named == "ssh";
    }
    !url_is_local(value)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_uri_component(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn posix_normalize(path: &str) -> String {
    let is_abs = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if !stack.is_empty() && stack.last() != Some(&"..") {
                stack.pop();
            } else if !is_abs {
                stack.push("..");
            }
        } else {
            stack.push(seg);
        }
    }
    if is_abs {
        format!("/{}", stack.join("/"))
    } else if stack.is_empty() {
        ".".to_owned()
    } else {
        stack.join("/")
    }
}

/// The filesystem path a local url names (v2.10): ONLY an absolute `file:///` url whose decoded
/// path is already normal (no `.`, `..` or empty segment, no trailing slash).
pub fn local_url_path(value: &str) -> Option<String> {
    if unsafe_url(value) || scheme(value) != Some("file") {
        return None;
    }
    let rest = &value["file://".len()..];
    if !rest.starts_with('/') || rest.contains(['?', '#']) {
        return None;
    }
    let path = decode_uri_component(rest)?;
    let normal = path.len() > 1
        && !path.contains('\0')
        && !path.ends_with('/')
        && posix_normalize(&path) == path;
    if !normal {
        return None;
    }
    let all_clean = path
        .split('/')
        .skip(1)
        .all(|part| !part.is_empty() && part != "." && part != "..");
    all_clean.then_some(path)
}

/// A url slot's value git would treat as a local repository (a file url or a path), which the hand
/// then judges strictly.
fn local_candidate(value: &str) -> bool {
    !unsafe_url(value)
        && (scheme(value) == Some("file") || (scheme(value).is_none() && url_is_local(value)))
}

fn is_ref_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-')
}

fn is_refspec(value: &str) -> bool {
    if value.contains("..") {
        return false;
    }
    let Some((left, right)) = value.split_once(':') else {
        return false;
    };
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if !right.bytes().all(is_ref_char) {
        return false;
    }
    // Left matches /^[A-Za-z0-9._/]+(?:-[A-Za-z0-9._/]+)*$/
    let left_segment = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/'))
    };
    left.split('-').all(left_segment)
}

fn is_iso_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 13 {
        return false;
    }
    // YYYY-MM-DDT...Z
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
    };
    if !(digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10)
        && bytes.get(10) == Some(&b'T')
        && bytes.last() == Some(&b'Z'))
    {
        return false;
    }
    let mid = &bytes[11..bytes.len() - 1];
    !mid.is_empty()
        && mid
            .iter()
            .all(|b| b.is_ascii_digit() || matches!(b, b':' | b'.'))
}

fn matches_kind(kind: Kind, value: &str) -> bool {
    match kind {
        Kind::Rev => is_rev(value),
        Kind::Range => is_range(value),
        Kind::Revish => is_rev(value) || is_range(value),
        Kind::Object => is_rev(value) || is_blobref(value),
        Kind::Blobref => is_blobref(value),
        Kind::Ref => {
            value == "HEAD"
                || (value.starts_with("refs/")
                    && value.len() > "refs/".len()
                    && value["refs/".len()..].bytes().all(is_ref_char)
                    && !value.contains(".."))
        }
        Kind::Refpat => {
            let trimmed = value.strip_suffix('*').unwrap_or(value);
            trimmed.starts_with("refs/")
                && trimmed.len() > "refs/".len()
                && trimmed["refs/".len()..].bytes().all(is_ref_char)
                && !value.contains("..")
        }
        Kind::Branch => {
            value
                .bytes()
                .next()
                .is_some_and(|first| !matches!(first, b'-' | b'.' | b'/'))
                && value.bytes().all(is_ref_char)
                && !value.contains("..")
        }
        Kind::Refspec => is_refspec(value),
        Kind::Msg | Kind::Fmt => !value.contains('\0'),
        Kind::Count => (1..=9).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit()),
        Kind::Mode => matches!(value, "100644" | "100755" | "120000" | "160000"),
        Kind::Oid => {
            (value.len() == 40 || value.len() == 64)
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        Kind::Filter => {
            !value.is_empty()
                && value.bytes().all(|b| {
                    matches!(
                        b.to_ascii_uppercase(),
                        b'A' | b'C' | b'D' | b'M' | b'R' | b'T' | b'U' | b'X' | b'B'
                    )
                })
        }
        Kind::Iso => is_iso_utc(value),
        Kind::Relpath => is_rel(value),
        Kind::Gitpath => value == "index" || value == "MERGE_HEAD",
        Kind::Path | Kind::Worktree => is_filesystem(value),
        Kind::Source | Kind::Target => is_remote_url(value) || local_candidate(value),
    }
}

pub fn shapes() -> &'static HashMap<&'static str, Vec<Vec<Item>>> {
    static SHAPES: OnceLock<HashMap<&'static str, Vec<Vec<Item>>>> = OnceLock::new();
    SHAPES.get_or_init(|| {
        let format = eq("--format=", Kind::Fmt);
        let log_record = "--format=%x1e%H%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%cI";
        let mut map = HashMap::new();
        map.insert(
            "add",
            vec![vec![lit("-A"), lit("--"), rep(1, vec![k(Kind::Path)])]],
        );
        map.insert(
            "branch",
            vec![
                vec![lit("--show-current")],
                vec![lit("-D"), k(Kind::Branch)],
            ],
        );
        map.insert(
            "cat-file",
            vec![
                vec![lit("blob"), k(Kind::Object)],
                vec![lit("--batch")],
                vec![lit("-e"), k(Kind::Blobref)],
            ],
        );
        map.insert(
            "checkout",
            vec![vec![k(Kind::Rev), lit("--"), k(Kind::Path)]],
        );
        map.insert(
            "commit",
            vec![vec![lit("-q"), lit("--no-verify"), lit("-m"), k(Kind::Msg)]],
        );
        map.insert(
            "commit-tree",
            vec![vec![
                k(Kind::Rev),
                opt(vec![lit("-p"), k(Kind::Rev)]),
                lit("-F"),
                lit("-"),
            ]],
        );
        map.insert("count-objects", vec![vec![lit("-v")]]);
        map.insert(
            "diff",
            vec![
                vec![
                    lit("--cached"),
                    lit("-M"),
                    lit("-C"),
                    lit("-z"),
                    lit("--raw"),
                    lit("--no-abbrev"),
                ],
                vec![
                    lit("--name-only"),
                    lit("-z"),
                    eq("--diff-filter=", Kind::Filter),
                    k(Kind::Rev),
                    k(Kind::Rev),
                    lit("--"),
                ],
                vec![
                    lit("--name-status"),
                    lit("-M"),
                    lit("-z"),
                    k(Kind::Range),
                    lit("--"),
                ],
                vec![lit("--cached"), lit("--quiet"), k(Kind::Rev), lit("--")],
                vec![
                    lit("--no-renames"),
                    lit("--name-only"),
                    lit("-z"),
                    k(Kind::Range),
                    lit("--"),
                    k(Kind::Path),
                ],
                vec![lit("--name-only"), lit("-z"), k(Kind::Range), lit("--")],
                vec![lit("--name-only"), eq("--diff-filter=", Kind::Filter)],
            ],
        );
        map.insert("diff-files", vec![vec![lit("--name-only"), lit("-z")]]);
        map.insert(
            "diff-tree",
            vec![vec![
                lit("-r"),
                lit("-m"),
                lit("--no-commit-id"),
                lit("--no-renames"),
                eq("--diff-filter=", Kind::Filter),
                lit("--raw"),
                lit("-z"),
                k(Kind::Rev),
            ]],
        );
        map.insert(
            "for-each-ref",
            vec![vec![format.clone(), rep(1, vec![k(Kind::Refpat)])]],
        );
        map.insert(
            "hash-object",
            vec![
                vec![opt(vec![lit("-w")]), lit("--no-filters"), lit("--stdin")],
                vec![
                    opt(vec![lit("-w")]),
                    eq("--path=", Kind::Relpath),
                    lit("--stdin"),
                ],
            ],
        );
        map.insert("init", vec![vec![lit("--template=")]]);
        map.insert(
            "log",
            vec![
                vec![lit("-1"), format.clone(), k(Kind::Rev)],
                vec![
                    Item::Prefixed {
                        prefix: "-",
                        kind: Kind::Count,
                    },
                    format.clone(),
                    k(Kind::Rev),
                ],
                vec![
                    format.clone(),
                    lit("--name-only"),
                    lit("-z"),
                    lit("--no-renames"),
                    lit("--diff-merges=first-parent"),
                    eq("--diff-filter=", Kind::Filter),
                    rep(0, vec![k(Kind::Revish)]),
                    lit("--"),
                ],
                vec![
                    lit(log_record),
                    opt(vec![
                        lit("--name-only"),
                        lit("-z"),
                        lit("--no-renames"),
                        lit("--full-diff"),
                    ]),
                    opt(vec![
                        lit("--fixed-strings"),
                        lit("--all-match"),
                        rep(1, vec![eq("--grep=", Kind::Msg)]),
                    ]),
                    opt(vec![lit("-n"), k(Kind::Count)]),
                    opt(vec![eq("--since=", Kind::Iso)]),
                    opt(vec![k(Kind::Revish)]),
                    opt(vec![lit("--"), rep(1, vec![k(Kind::Path)])]),
                ],
            ],
        );
        map.insert(
            "ls-files",
            vec![
                vec![lit("--stage"), lit("-z"), lit("--"), k(Kind::Path)],
                vec![lit("--eol"), lit("-z")],
            ],
        );
        map.insert("ls-remote", vec![vec![k(Kind::Source), k(Kind::Ref)]]);
        map.insert(
            "ls-tree",
            vec![
                vec![
                    lit("-r"),
                    lit("--name-only"),
                    lit("-z"),
                    k(Kind::Rev),
                    opt(vec![lit("--"), k(Kind::Path)]),
                ],
                vec![lit("-r"), lit("-l"), lit("-z"), k(Kind::Rev)],
                vec![lit("-r"), lit("-z"), k(Kind::Rev)],
                vec![lit("-z"), k(Kind::Rev), lit("--"), k(Kind::Path)],
            ],
        );
        map.insert(
            "maintenance",
            vec![vec![
                lit("run"),
                lit("--task=loose-objects"),
                lit("--quiet"),
            ]],
        );
        map.insert(
            "merge",
            vec![
                vec![],
                vec![lit("--abort")],
                vec![
                    opt(vec![lit("--no-ff")]),
                    opt(vec![lit("-m"), k(Kind::Msg)]),
                    k(Kind::Rev),
                ],
                vec![
                    lit("--no-ff"),
                    k(Kind::Rev),
                    rep(1, vec![lit("-m"), k(Kind::Msg)]),
                ],
            ],
        );
        map.insert(
            "merge-base",
            vec![vec![lit("--is-ancestor"), k(Kind::Rev), k(Kind::Rev)]],
        );
        map.insert("prune-packed", vec![vec![lit("--quiet")]]);
        map.insert(
            "push",
            vec![vec![lit("--quiet"), k(Kind::Target), k(Kind::Refspec)]],
        );
        map.insert("read-tree", vec![vec![lit("--empty")], vec![k(Kind::Rev)]]);
        map.insert("remote", vec![vec![lit("-v")]]);
        map.insert("reset", vec![vec![lit("--soft"), k(Kind::Rev)]]);
        map.insert(
            "restore",
            vec![vec![
                lit("--source=HEAD"),
                lit("--staged"),
                opt(vec![lit("--worktree")]),
                lit("--"),
                rep(1, vec![k(Kind::Path)]),
            ]],
        );
        map.insert(
            "rev-list",
            vec![
                vec![opt(vec![lit("--count")]), k(Kind::Range)],
                vec![lit("--reverse"), k(Kind::Range)],
            ],
        );
        map.insert(
            "rev-parse",
            vec![
                vec![lit("--verify"), opt(vec![lit("--quiet")]), k(Kind::Rev)],
                vec![lit("-q"), lit("--verify"), k(Kind::Rev)],
                vec![lit("--git-path"), k(Kind::Gitpath)],
                vec![lit("--git-common-dir")],
            ],
        );
        map.insert(
            "rm",
            vec![
                vec![
                    lit("-q"),
                    lit("--cached"),
                    opt(vec![lit("-f")]),
                    lit("--ignore-unmatch"),
                    lit("--"),
                    rep(1, vec![k(Kind::Path)]),
                ],
                vec![
                    lit("-rf"),
                    lit("-q"),
                    lit("--ignore-unmatch"),
                    lit("--"),
                    k(Kind::Path),
                ],
            ],
        );
        map.insert(
            "show",
            vec![
                vec![lit("-s"), format, k(Kind::Rev)],
                vec![k(Kind::Blobref)],
            ],
        );
        map.insert("show-ref", vec![vec![lit("--verify"), k(Kind::Ref)]]);
        map.insert(
            "status",
            vec![vec![
                lit("--porcelain"),
                opt(vec![
                    lit("--untracked-files=all"),
                    opt(vec![lit("--"), rep(1, vec![k(Kind::Path)])]),
                ]),
            ]],
        );
        map.insert("symbolic-ref", vec![vec![lit("HEAD"), k(Kind::Ref)]]);
        map.insert(
            "update-index",
            vec![
                vec![lit("--force-remove"), lit("--"), k(Kind::Path)],
                vec![
                    lit("--add"),
                    lit("--cacheinfo"),
                    k(Kind::Mode),
                    k(Kind::Oid),
                    k(Kind::Path),
                ],
                vec![lit("-q"), lit("-z"), lit("--stdin")],
            ],
        );
        map.insert(
            "update-ref",
            vec![vec![
                lit("-m"),
                k(Kind::Msg),
                lit("HEAD"),
                k(Kind::Rev),
                k(Kind::Rev),
            ]],
        );
        map.insert(
            "worktree",
            vec![
                vec![
                    lit("add"),
                    lit("-b"),
                    k(Kind::Branch),
                    k(Kind::Worktree),
                    k(Kind::Rev),
                ],
                vec![lit("remove"), opt(vec![lit("--force")]), k(Kind::Worktree)],
                vec![lit("list"), lit("--porcelain")],
                vec![lit("prune")],
            ],
        );
        map.insert("write-tree", vec![vec![]]);
        map
    })
}

fn match_items(
    items: &[Item],
    args: &[String],
    at: usize,
    paths: &mut Vec<TypedPath>,
    then: &dyn Fn(usize, &mut Vec<TypedPath>) -> bool,
) -> bool {
    let Some((item, rest)) = items.split_first() else {
        return then(at, paths);
    };
    match item {
        Item::Literal(lit) => {
            args.get(at).is_some_and(|a| a == lit) && match_items(rest, args, at + 1, paths, then)
        }
        Item::Optional(opt_items) => {
            match_items(opt_items, args, at, paths, &|to, p| {
                match_items(rest, args, to, p, then)
            }) || match_items(rest, args, at, paths, then)
        }
        Item::Repeat {
            items: rep_items,
            min,
        } => {
            #[allow(clippy::too_many_arguments)]
            fn again(
                rep_items: &[Item],
                min: usize,
                rest: &[Item],
                args: &[String],
                count: usize,
                from: usize,
                paths: &mut Vec<TypedPath>,
                then: &dyn Fn(usize, &mut Vec<TypedPath>) -> bool,
            ) -> bool {
                if count >= min && match_items(rest, args, from, paths, then) {
                    return true;
                }
                match_items(rep_items, args, from, paths, &|to, p| {
                    to > from && again(rep_items, min, rest, args, count + 1, to, p, then)
                })
            }
            again(rep_items, *min, rest, args, 0, at, paths, then)
        }
        Item::Kind(kind) => {
            let Some(arg) = args.get(at) else {
                return false;
            };
            if !matches_kind(*kind, arg) {
                return false;
            }
            let typed_kind = match kind {
                Kind::Path => Some(TypedPathKind::Path),
                Kind::Worktree => Some(TypedPathKind::Worktree),
                Kind::Source if local_candidate(arg) => Some(TypedPathKind::Source),
                Kind::Target if local_candidate(arg) => Some(TypedPathKind::Target),
                _ => None,
            };
            if let Some(tk) = typed_kind {
                paths.push(TypedPath {
                    kind: tk,
                    value: arg.clone(),
                });
            }
            if match_items(rest, args, at + 1, paths, then) {
                return true;
            }
            if typed_kind.is_some() {
                paths.pop();
            }
            false
        }
        Item::Prefixed { prefix, kind } => {
            let Some(arg) = args.get(at) else {
                return false;
            };
            let Some(value) = arg.strip_prefix(prefix) else {
                return false;
            };
            if !matches_kind(*kind, value) {
                return false;
            }
            let typed_kind = match kind {
                Kind::Path => Some(TypedPathKind::Path),
                Kind::Worktree => Some(TypedPathKind::Worktree),
                Kind::Source if local_candidate(value) => Some(TypedPathKind::Source),
                Kind::Target if local_candidate(value) => Some(TypedPathKind::Target),
                _ => None,
            };
            if let Some(tk) = typed_kind {
                paths.push(TypedPath {
                    kind: tk,
                    value: value.to_owned(),
                });
            }
            if match_items(rest, args, at + 1, paths, then) {
                return true;
            }
            if typed_kind.is_some() {
                paths.pop();
            }
            false
        }
    }
}

/// The path arguments of the first shape `args` matches exactly, or `None` when no shape admits
/// them.
pub fn match_shape(sub: &str, args: &[String]) -> Option<Vec<TypedPath>> {
    let sub_shapes = shapes().get(sub)?;
    for shape in sub_shapes {
        let mut paths = Vec::new();
        if match_items(shape, args, 0, &mut paths, &|at, _| at == args.len()) {
            return Some(paths);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn admits_valid_shapes_and_extracts_typed_paths() {
        let paths = match_shape("add", &s(&["-A", "--", "a.txt", "b/c.txt"])).expect("add");
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].kind, TypedPathKind::Path);

        let wt = match_shape("worktree", &s(&["add", "-b", "feat/x", "../wt", "HEAD"]))
            .expect("worktree add");
        assert_eq!(
            wt,
            vec![TypedPath {
                kind: TypedPathKind::Worktree,
                value: "../wt".to_owned(),
            }]
        );

        let push_local = match_shape("push", &s(&["--quiet", "file:///tmp/mirror", "main:main"]))
            .expect("push local");
        assert_eq!(
            push_local,
            vec![TypedPath {
                kind: TypedPathKind::Target,
                value: "file:///tmp/mirror".to_owned(),
            }]
        );

        let push_remote = match_shape(
            "push",
            &s(&["--quiet", "https://example.com/repo.git", "main:main"]),
        )
        .expect("push remote");
        assert!(push_remote.is_empty());
    }

    #[test]
    fn rejects_option_like_paths_and_malformed_urls() {
        assert!(match_shape("add", &s(&["-A", "--", "--upload-pack=evil"])).is_none());
        assert!(match_shape("ls-remote", &s(&["ext::sh -c id", "HEAD"])).is_none());
        assert_eq!(
            local_url_path("file:///tmp/repo"),
            Some("/tmp/repo".to_owned())
        );
        assert_eq!(local_url_path("file:///tmp/../etc"), None);
        assert_eq!(local_url_path("file:///tmp/repo/"), None);
        assert_eq!(local_url_path("file:///tmp/repo?x=1"), None);
        assert_eq!(local_url_path("/tmp/repo"), None);
    }
}
