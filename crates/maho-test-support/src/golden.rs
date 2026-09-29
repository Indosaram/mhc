//! Byte-exact comparison against golden fixtures.
//!
//! Fixtures are produced only by `tools/golden/run.mjs` from the pinned senpi source. This module
//! never writes them: a mismatch is a failure with a unified diff, and the fix is either the Rust
//! code or a regenerated fixture from senpi.

use std::fmt;
use std::path::{Path, PathBuf};

use similar::TextDiff;

/// Absolute path of the workspace root (two levels above this crate).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .components()
        .collect()
}

/// Path of `crates/<crate_name>/tests/golden/<file>` in this workspace.
pub fn fixture_path(crate_name: &str, file: &str) -> PathBuf {
    workspace_root()
        .join("crates")
        .join(crate_name)
        .join("tests")
        .join("golden")
        .join(file)
}

/// Why a golden comparison failed.
#[derive(Debug)]
pub enum GoldenError {
    /// The fixture could not be read (missing fixtures are never created here).
    Read { path: PathBuf, source: std::io::Error },
    /// The bytes differ; `diff` is a unified diff from expected (fixture) to actual.
    Mismatch { path: PathBuf, diff: String },
}

impl fmt::Display for GoldenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(
                f,
                "golden fixture {} unreadable: {source} (generate it with bun tools/golden/run.mjs)",
                path.display()
            ),
            Self::Mismatch { path, diff } => {
                write!(f, "golden mismatch against {}\n{diff}", path.display())
            }
        }
    }
}

impl std::error::Error for GoldenError {}

/// Compares `actual` with the fixture at `path` byte for byte.
pub fn compare(path: &Path, actual: &str) -> Result<(), GoldenError> {
    let expected = std::fs::read(path).map_err(|source| GoldenError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if expected == actual.as_bytes() {
        return Ok(());
    }
    Err(GoldenError::Mismatch {
        path: path.to_path_buf(),
        diff: unified_diff(&String::from_utf8_lossy(&expected), actual),
    })
}

/// Compares rendered lines, joined with `\n` exactly as the generator writes them.
pub fn compare_lines<S: AsRef<str>>(path: &Path, lines: &[S]) -> Result<(), GoldenError> {
    let joined = lines.iter().map(AsRef::as_ref).collect::<Vec<_>>().join("\n");
    compare(path, &joined)
}

/// Panicking form for tests: prints the unified diff on mismatch.
#[track_caller]
pub fn assert_golden(path: &Path, actual: &str) {
    if let Err(error) = compare(path, actual) {
        panic!("{error}");
    }
}

/// Panicking form of [`compare_lines`].
#[track_caller]
pub fn assert_golden_lines<S: AsRef<str>>(path: &Path, lines: &[S]) {
    if let Err(error) = compare_lines(path, lines) {
        panic!("{error}");
    }
}

/// Unified diff with escaped control bytes so ANSI differences stay visible.
pub fn unified_diff(expected: &str, actual: &str) -> String {
    let expected = escape_controls(expected);
    let actual = escape_controls(actual);
    TextDiff::from_lines(&expected, &actual)
        .unified_diff()
        .context_radius(3)
        .header("expected (golden)", "actual")
        .to_string()
}

fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push('\n'),
            '\x1b' => out.push_str("\\e"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
