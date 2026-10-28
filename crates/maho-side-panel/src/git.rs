//! `git status`/numstat parsing, the throttled refresh and diff commands (spec 5 Files, 6, 7).

use std::collections::BTreeMap;
use std::time::Duration;

/// Rows shown in the files section.
pub const SHOWN: usize = 8;
/// Minimum interval between unforced git refreshes.
pub const THROTTLE: Duration = Duration::from_secs(2);
/// Timeout of each git command.
pub const TIMEOUT_MS: u64 = 5_000;
/// Diff output line cap.
pub const DIFF_LINE_CAP: usize = 5_000;
/// Untracked files listed for an untracked directory.
pub const UNTRACKED_DIR_CAP: usize = 50;

/// Arguments of the status command (porcelain v1, NUL separated, untracked files normal).
pub fn status_args() -> Vec<String> {
    ["status", "--porcelain=v1", "-z", "--untracked-files=normal"]
        .map(String::from)
        .to_vec()
}

/// Arguments of the numstat commands: staged then unstaged.
pub fn numstat_args(staged: bool) -> Vec<String> {
    let mut args: Vec<String> = ["diff", "--numstat", "-z", "-M"].map(String::from).to_vec();
    if staged {
        args.insert(1, "--cached".into());
    }
    args
}

/// Color class of a changed file row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileClass {
    Untracked,
    Conflict,
    StagedOnly,
    Normal,
}

/// One changed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    /// Two-letter status code `XY`.
    pub xy: String,
    /// Path (rename destination for renames).
    pub path: String,
    /// Rename source.
    pub from: Option<String>,
    /// Added/removed lines (staged + unstaged); `None` for untracked files.
    pub counts: Option<(u64, u64)>,
}

impl ChangedFile {
    pub fn untracked(&self) -> bool {
        self.xy == "??"
    }

    pub fn class(&self) -> FileClass {
        let b = self.xy.as_bytes();
        let (x, y) = (
            b.first().copied().unwrap_or(b' '),
            b.get(1).copied().unwrap_or(b' '),
        );
        if self.untracked() {
            FileClass::Untracked
        } else if x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D') {
            FileClass::Conflict
        } else if x != b' ' && y == b' ' {
            FileClass::StagedOnly
        } else {
            FileClass::Normal
        }
    }

    pub fn basename(&self) -> &str {
        let trimmed = self.path.trim_end_matches('/');
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }

    /// `+A/-R`, or empty for untracked files.
    pub fn counts_label(&self) -> String {
        self.counts
            .map(|(a, r)| format!(" +{a}/-{r}"))
            .unwrap_or_default()
    }

    /// Picker label `<XY> <path>[ +A/-R]`.
    pub fn label(&self) -> String {
        format!("{} {}{}", self.xy, self.path, self.counts_label())
    }
}

/// Parses `git status --porcelain=v1 -z` output.
pub fn parse_status(out: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut parts = out.split('\0').filter(|p| !p.is_empty());
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let xy = entry[..2].to_owned();
        let path = entry[3..].to_owned();
        let from = if xy.contains('R') || xy.contains('C') {
            parts.next().map(str::to_owned)
        } else {
            None
        };
        files.push(ChangedFile {
            xy,
            path,
            from,
            counts: None,
        });
    }
    files
}

/// Parses `git diff --numstat -z` output into `path -> (added, removed)`; binary files are 0/0.
pub fn parse_numstat(out: &str) -> BTreeMap<String, (u64, u64)> {
    let mut map = BTreeMap::new();
    let mut parts = out.split('\0');
    while let Some(entry) = parts.next() {
        if entry.is_empty() {
            continue;
        }
        let entry = entry.trim_start_matches('\n');
        let mut fields = entry.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            // Rename: the next two NUL fields are source and destination.
            let _from = parts.next();
            parts.next().unwrap_or_default().to_owned()
        } else {
            path.to_owned()
        };
        let a = a.parse().unwrap_or(0);
        let r = r.parse().unwrap_or(0);
        let slot = map.entry(path).or_insert((0, 0));
        slot.0 += a;
        slot.1 += r;
    }
    map
}

/// Combines status with staged and unstaged numstat.
pub fn merge(status: &str, staged: &str, unstaged: &str) -> Vec<ChangedFile> {
    let mut files = parse_status(status);
    let staged = parse_numstat(staged);
    let unstaged = parse_numstat(unstaged);
    for file in &mut files {
        if file.untracked() {
            continue;
        }
        let s = staged.get(&file.path).copied().unwrap_or((0, 0));
        let u = unstaged.get(&file.path).copied().unwrap_or((0, 0));
        file.counts = Some((s.0 + u.0, s.1 + u.1));
    }
    files
}

/// Throttle state for refreshes, on the host's millisecond clock.
#[derive(Debug, Default)]
pub struct Throttle {
    last: Option<u64>,
}

impl Throttle {
    /// Whether a refresh may run at `now_ms`; records the run when it may.
    pub fn admit(&mut self, now_ms: u64, interval: Duration, forced: bool) -> bool {
        let interval = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX);
        if !forced
            && self
                .last
                .is_some_and(|last| now_ms.saturating_sub(last) < interval)
        {
            return false;
        }
        self.last = Some(now_ms);
        true
    }
}

/// Arguments of the diff for a changed file (spec 7).
pub fn diff_args(file: &ChangedFile) -> Vec<String> {
    if file.untracked() {
        return ["diff", "--no-index", "--", "/dev/null", file.path.as_str()]
            .map(String::from)
            .to_vec();
    }
    let mut args: Vec<String> = ["--literal-pathspecs", "diff", "HEAD", "-M", "--"]
        .map(String::from)
        .to_vec();
    if let Some(from) = &file.from {
        args.push(from.clone());
    }
    args.push(file.path.clone());
    args
}

/// Arguments listing untracked files under a directory.
pub fn untracked_dir_args(dir: &str) -> Vec<String> {
    vec![
        "--literal-pathspecs".into(),
        "ls-files".into(),
        "--others".into(),
        "--exclude-standard".into(),
        "-z".into(),
        "--".into(),
        dir.into(),
    ]
}

/// One row of the diff viewer body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Line(String),
    More(usize),
    NoChange,
    Error(i32),
}

impl DiffRow {
    pub fn text(&self) -> String {
        match self {
            Self::Line(line) => line.clone(),
            Self::More(n) => format!("... {n} more lines"),
            Self::NoChange => "(no textual change)".into(),
            Self::Error(code) => format!("git diff failed (exit code {code})"),
        }
    }
}

/// Builds viewer rows from diff output. `--no-index` exits 1 when files differ; that is success.
pub fn diff_rows(stdout: &str, code: i32, no_index: bool) -> Vec<DiffRow> {
    let ok = code == 0 || (no_index && code == 1);
    if !ok {
        return vec![DiffRow::Error(code)];
    }
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return vec![DiffRow::NoChange];
    }
    let mut rows: Vec<DiffRow> = lines
        .iter()
        .take(DIFF_LINE_CAP)
        .map(|l| DiffRow::Line((*l).to_owned()))
        .collect();
    if lines.len() > DIFF_LINE_CAP {
        rows.push(DiffRow::More(lines.len() - DIFF_LINE_CAP));
    }
    rows
}

/// Rows listing untracked files under a directory, capped with a count of the rest.
pub fn untracked_dir_rows(stdout: &str) -> Vec<DiffRow> {
    let files: Vec<&str> = stdout.split('\0').filter(|p| !p.is_empty()).collect();
    if files.is_empty() {
        return vec![DiffRow::NoChange];
    }
    let mut rows: Vec<DiffRow> = files
        .iter()
        .take(UNTRACKED_DIR_CAP)
        .map(|f| DiffRow::Line(format!("?? {f}")))
        .collect();
    if files.len() > UNTRACKED_DIR_CAP {
        rows.push(DiffRow::Line(format!(
            "... {} more untracked files",
            files.len() - UNTRACKED_DIR_CAP
        )));
    }
    rows
}

/// Branch of a worktree: reads `.git` (a directory, or a file pointing at a linked worktree's git
/// dir) and its `HEAD`. Detached HEAD yields the 7-char commit.
pub fn branch(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return head_label(&dot_git);
        }
        if dot_git.is_file() {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let path = std::path::Path::new(target);
            let git_dir = if path.is_absolute() {
                path.to_path_buf()
            } else {
                current.join(path)
            };
            return head_label(&git_dir);
        }
        dir = current.parent();
    }
    None
}

fn head_label(git_dir: &std::path::Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        return Some(
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_owned(),
        );
    }
    (head.len() >= 7).then(|| head[..7].to_owned())
}
