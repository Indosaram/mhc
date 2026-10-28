//! Spec §3: formatter choice from the entries directly in the session cwd, and the prettier ->
//! `bun format` substitution.
use crate::run::{Exit, Host};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A detected formatter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatterKind {
    Biome,
    Prettier,
    BunFormat,
    Rustfmt,
    Gofmt,
    Ruff,
}

impl FormatterKind {
    /// Name shown to the model and in warnings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Biome => "biome",
            Self::Prettier => "prettier",
            Self::BunFormat => "bun format",
            Self::Rustfmt => "rustfmt",
            Self::Gofmt => "gofmt",
            Self::Ruff => "ruff",
        }
    }

    /// Program and arguments before the file path.
    pub fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Biome => ("biome", &["format", "--write"]),
            Self::Prettier => ("prettier", &["--write"]),
            Self::BunFormat => ("bun", &["format", "--write"]),
            Self::Rustfmt => ("rustfmt", &[]),
            Self::Gofmt => ("gofmt", &["-w"]),
            Self::Ruff => ("ruff", &["format"]),
        }
    }
}

/// Which formatter families the cwd's markers allow. Computed once per tool result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Markers {
    pub biome: bool,
    pub prettier: bool,
    pub rustfmt: bool,
    pub gofmt: bool,
    pub ruff: bool,
}

fn has_ruff_section(pyproject: &Path) -> bool {
    std::fs::read_to_string(pyproject).is_ok_and(|text| {
        text.lines()
            .any(|line| line.trim_start_matches(' ').starts_with("[tool.ruff]"))
    })
}

impl Markers {
    /// Looks only at the names of the entries directly in `cwd`.
    pub fn scan(cwd: &Path) -> Self {
        let mut markers = Self::default();
        let Ok(entries) = std::fs::read_dir(cwd) else {
            return markers;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            match name.as_str() {
                "biome.json" | "biome.jsonc" => markers.biome = true,
                "rustfmt.toml" | "Cargo.toml" => markers.rustfmt = true,
                "go.mod" => markers.gofmt = true,
                "ruff.toml" => markers.ruff = true,
                "pyproject.toml" => {
                    markers.ruff |= has_ruff_section(&entry.path());
                }
                _ if name.starts_with(".prettierrc") || name.starts_with("prettier.config.") => {
                    markers.prettier = true;
                }
                _ => {}
            }
        }
        markers
    }

    /// First matching row for `file` (before the Bun substitution).
    pub fn choose(&self, file: &Path) -> Option<FormatterKind> {
        let extension = file
            .extension()
            .map(|ext| ext.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.biome {
            Some(FormatterKind::Biome)
        } else if self.prettier {
            Some(FormatterKind::Prettier)
        } else if self.rustfmt && extension == "rs" {
            Some(FormatterKind::Rustfmt)
        } else if self.gofmt && extension == "go" {
            Some(FormatterKind::Gofmt)
        } else if self.ruff && extension == "py" {
            Some(FormatterKind::Ruff)
        } else {
            None
        }
    }
}

const BUN_MARKERS: [&str; 3] = ["bun.lock", "bun.lockb", "bunfig.toml"];
const JS_FAMILY: [&str; 8] = ["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];
const BUN_SHIM_SUFFIXES: [&str; 4] = ["", ".exe", ".bunx", ".cmd"];
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_SOURCE: &str = "const   probe = {a:1,\n   b:2}\n";

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Walks up from `cwd`: "no" at the home directory or a directory holding `.git`; "yes" at a
/// directory with a Bun lock/config file or a `package.json` whose `packageManager` is `bun@...`.
pub fn is_bun_project(cwd: &Path, home: Option<&Path>) -> bool {
    for directory in cwd.ancestors() {
        if BUN_MARKERS.iter().any(|name| directory.join(name).exists()) {
            return true;
        }
        let manager = read_json(&directory.join("package.json")).and_then(|json| {
            json.get("packageManager")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        if manager.is_some_and(|manager| manager.starts_with("bun@")) {
            return true;
        }
        if home.is_some_and(|home| home == directory) || directory.join(".git").exists() {
            return false;
        }
    }
    false
}

fn nearest_package_json(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|directory| directory.join("package.json"))
        .find(|path| path.is_file())
}

fn js_family_file(directory: &Path, stem: &str) -> bool {
    JS_FAMILY
        .iter()
        .any(|ext| directory.join(format!("{stem}.{ext}")).is_file())
}

/// Something in the project that `bun format` would run instead of Bun's formatter.
pub fn has_format_shadow(cwd: &Path) -> bool {
    let script = nearest_package_json(cwd)
        .and_then(|path| read_json(&path))
        .is_some_and(|json| {
            json.get("scripts")
                .and_then(|scripts| scripts.get("format"))
                .is_some()
        });
    if script {
        return true;
    }
    let bin_shadow = cwd.ancestors().any(|directory| {
        let bin = directory.join("node_modules").join(".bin");
        BUN_SHIM_SUFFIXES
            .iter()
            .any(|suffix| bin.join(format!("format{suffix}")).is_file())
    });
    if bin_shadow {
        return true;
    }
    if cwd.join("format").is_file() || js_family_file(cwd, "format") {
        return true;
    }
    let folder = cwd.join("format");
    folder.is_dir() && (folder.join("package.json").is_file() || js_family_file(&folder, "index"))
}

/// `bun` resolves and, in a fresh temp directory, `bun format --help` succeeds and
/// `bun format --write` rewrites an unformatted TypeScript file.
pub async fn bun_formats(host: &Host, cwd: &Path) -> bool {
    let Some(bun) = host.resolve_binary(cwd, "bun") else {
        return false;
    };
    let Ok(dir) = tempfile::tempdir() else {
        return false;
    };
    let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    if host
        .run(&bun, &args(&["format", "--help"]), dir.path(), PROBE_TIMEOUT)
        .await
        != Exit::Success
    {
        return false;
    }
    let probe = dir.path().join("probe.ts");
    if std::fs::write(&probe, PROBE_SOURCE).is_err() {
        return false;
    }
    let mut write = args(&["format", "--write"]);
    write.push(probe.as_os_str().to_owned());
    if host.run(&bun, &write, dir.path(), PROBE_TIMEOUT).await != Exit::Success {
        return false;
    }
    std::fs::read_to_string(&probe).is_ok_and(|after| after != PROBE_SOURCE)
}

/// The prettier row after the Bun substitution: `bun format` when the project is a Bun project,
/// nothing shadows `format`, and Bun proves it formats; prettier otherwise.
pub async fn prettier_or_bun(host: &Host, cwd: &Path) -> FormatterKind {
    if is_bun_project(cwd, host.home_dir().as_deref())
        && !has_format_shadow(cwd)
        && bun_formats(host, cwd).await
    {
        FormatterKind::BunFormat
    } else {
        FormatterKind::Prettier
    }
}
