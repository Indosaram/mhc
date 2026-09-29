//! List zip archive entries through system tools (python3, PowerShell, tar, zipinfo) for validation.

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::archive_entry_validator::{ArchiveEntry, ArchiveEntryType};
use crate::logger::log;
use crate::runtime::{SpawnOptions, StdioMode, spawn, spawn_sync};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipListingError {
    pub message: String,
}

impl fmt::Display for ZipListingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ZipListingError {}

fn listing_error(message: String) -> ZipListingError {
    ZipListingError { message }
}

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("{error}"))
}

fn piped() -> SpawnOptions {
    SpawnOptions {
        stdout: Some(StdioMode::Pipe),
        stderr: Some(StdioMode::Pipe),
        ..Default::default()
    }
}

fn run_listing(cmd: &[&str], failure: &str) -> Result<String, ZipListingError> {
    let argv: Vec<String> = cmd.iter().map(|part| (*part).to_string()).collect();
    let process = spawn(&argv, &piped())
        .map_err(|error| listing_error(format!("{failure} (spawn): {error}")))?;
    let (code, stdout, stderr) = process
        .wait_with_output()
        .map_err(|error| listing_error(format!("{failure}: {error}")))?;
    if code != 0 {
        return Err(listing_error(format!("{failure} (exit {code}): {stderr}")));
    }
    Ok(stdout)
}

fn quiet_success(cmd: &[&str]) -> bool {
    let argv: Vec<String> = cmd.iter().map(|part| (*part).to_string()).collect();
    let options = SpawnOptions {
        stdout: Some(StdioMode::Ignore),
        stderr: Some(StdioMode::Ignore),
        ..Default::default()
    };
    spawn_sync(&argv, &options).is_ok_and(|result| result.exit_code == 0)
}

fn entry(path: &str, entry_type: ArchiveEntryType, link_path: Option<String>) -> ArchiveEntry {
    ArchiveEntry {
        path: path.to_string(),
        entry_type,
        link_path,
    }
}

pub fn is_python_zip_listing_available() -> bool {
    quiet_success(&["python3", "--version"])
}

const PYTHON_LISTING_SCRIPT: &str = "import json, stat, sys, zipfile
entries = []
with zipfile.ZipFile(sys.argv[1], 'r') as archive:
    for info in archive.infolist():
        mode = (info.external_attr >> 16) & 0xFFFF
        if stat.S_ISLNK(mode):
            entry_type = 'symlink'
            link_path = archive.read(info).decode('utf-8', 'surrogateescape')
        elif info.filename.endswith('/'):
            entry_type = 'directory'
            link_path = None
        else:
            entry_type = 'file'
            link_path = None
        entry = {'path': info.filename, 'type': entry_type}
        if link_path is not None:
            entry['linkPath'] = link_path
        entries.append(entry)
print(json.dumps(entries))";

fn entry_type_named(name: &str) -> Option<ArchiveEntryType> {
    match name {
        "file" => Some(ArchiveEntryType::File),
        "directory" => Some(ArchiveEntryType::Directory),
        "symlink" => Some(ArchiveEntryType::Symlink),
        "hardlink" => Some(ArchiveEntryType::Hardlink),
        _ => None,
    }
}

pub fn list_zip_entries_with_python(
    archive_path: &str,
) -> Result<Vec<ArchiveEntry>, ZipListingError> {
    let stdout = run_listing(
        &["python3", "-c", PYTHON_LISTING_SCRIPT, archive_path],
        "zip entry listing failed",
    )?;
    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|error| listing_error(format!("zip entry listing failed: {error}")))?;
    Ok(parsed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let path = item.get("path")?.as_str()?;
            let entry_type = entry_type_named(item.get("type")?.as_str()?)?;
            let link = item
                .get("linkPath")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(entry(path, entry_type, link))
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerShellZipExtractor {
    Pwsh,
    PowerShell,
}

impl PowerShellZipExtractor {
    pub const fn binary(self) -> &'static str {
        match self {
            PowerShellZipExtractor::Pwsh => "pwsh",
            PowerShellZipExtractor::PowerShell => "powershell",
        }
    }
}

/// Parse one `{type, name, target}` JSON line emitted by the PowerShell lister; `None` for other shapes.
pub fn parse_power_shell_zip_entry_line(
    line: &str,
) -> Result<Option<ArchiveEntry>, ZipListingError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| listing_error(error.to_string()))?;
    let (Some(kind), Some(name), Some(target)) = (
        value.get("type").and_then(Value::as_str),
        value.get("name").and_then(Value::as_str),
        value.get("target").and_then(Value::as_str),
    ) else {
        return Ok(None);
    };
    Ok(match kind {
        "symlink" => Some(entry(
            name,
            ArchiveEntryType::Symlink,
            Some(target.to_string()),
        )),
        "file" => Some(entry(name, ArchiveEntryType::File, None)),
        "directory" => Some(entry(name, ArchiveEntryType::Directory, None)),
        _ => None,
    })
}

pub fn list_zip_entries_with_power_shell(
    archive_path: &str,
    escape_power_shell_path: &dyn Fn(&str) -> String,
    extractor: PowerShellZipExtractor,
) -> Result<Vec<ArchiveEntry>, ZipListingError> {
    let script = [
        "Add-Type -AssemblyName System.IO.Compression.FileSystem".to_string(),
        format!("$archive = [System.IO.Compression.ZipFile]::OpenRead('{}')", escape_power_shell_path(archive_path)),
        "try {".to_string(),
        "  foreach ($entry in $archive.Entries) {".to_string(),
        "    $mode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF".to_string(),
        "    $type = if (($mode -band 0xF000) -eq 0xA000) { 'symlink' } elseif ($entry.FullName.EndsWith('/')) { 'directory' } else { 'file' }".to_string(),
        "    $target = ''".to_string(),
        "    if ($type -eq 'symlink') {".to_string(),
        "      $stream = $entry.Open()".to_string(),
        "      try {".to_string(),
        "        $reader = New-Object System.IO.StreamReader($stream)".to_string(),
        "        try { $target = $reader.ReadToEnd() } finally { $reader.Dispose() }".to_string(),
        "      } finally { $stream.Dispose() }".to_string(),
        "    }".to_string(),
        "    Write-Output (ConvertTo-Json @{type=$type; name=$entry.FullName; target=$target} -Compress)".to_string(),
        "  }".to_string(),
        "} finally {".to_string(),
        "  $archive.Dispose()".to_string(),
        "}".to_string(),
    ]
    .join("; ");
    let stdout = run_listing(
        &[extractor.binary(), "-Command", &script],
        "zip entry listing failed",
    )?;
    let mut entries = Vec::new();
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(parsed) = parse_power_shell_zip_entry_line(line)? {
            entries.push(parsed);
        }
    }
    Ok(entries)
}

static TAR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"^([^\s])\S*\s+\d+\s+\S+\s+\S+\s+\d+\s+\w+\s+\d+\s+(?:\d{2}:\d{2}|\d{4})\s+(.*)$")
});

fn parse_tar_listed_zip_entry(line: &str) -> Option<ArchiveEntry> {
    let captures = TAR_LINE.captures(line)?;
    let raw_type = captures.get(1)?.as_str();
    let raw_path = captures.get(2)?.as_str();
    match raw_type {
        "l" | "h" => {
            let entry_type = if raw_type == "l" {
                ArchiveEntryType::Symlink
            } else {
                ArchiveEntryType::Hardlink
            };
            Some(match raw_path.rfind(" -> ") {
                Some(index) => entry(
                    &raw_path[..index],
                    entry_type,
                    Some(raw_path[index + 4..].to_string()),
                ),
                None => entry(raw_path, entry_type, None),
            })
        }
        "d" => Some(entry(raw_path, ArchiveEntryType::Directory, None)),
        _ => Some(entry(raw_path, ArchiveEntryType::File, None)),
    }
}

/// Parse `tar -tvf` output; any unparsed line fails the whole listing (fail-closed).
pub fn parse_tar_listing_output(stdout: &str) -> Result<Vec<ArchiveEntry>, ZipListingError> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut entries = Vec::new();
    let mut unparsed = 0;
    for line in &lines {
        match parse_tar_listed_zip_entry(line) {
            Some(parsed) => entries.push(parsed),
            None => {
                unparsed += 1;
                log(
                    "warning: unparsed tar listing line",
                    Some(&json!({ "line": line })),
                );
            }
        }
    }
    if unparsed > 0 {
        return Err(listing_error(format!(
            "zip entry listing failed: {unparsed}/{} tar listing lines could not be parsed (fail-closed)",
            lines.len()
        )));
    }
    Ok(entries)
}

pub fn list_zip_entries_with_tar(archive_path: &str) -> Result<Vec<ArchiveEntry>, ZipListingError> {
    parse_tar_listing_output(&run_listing(
        &["tar", "-tvf", archive_path],
        "zip entry listing failed",
    )?)
}

static ZIPINFO_LINE: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"^([-dl?])\S*\s+\S+\s+\S+\s+\d+\s+\S+\s+\d+\s+\S+\s+\S+\s+\S+\s+(.*)$")
});

pub fn parse_zip_info_listed_entry(line: &str) -> Option<ArchiveEntry> {
    let captures = ZIPINFO_LINE.captures(line)?;
    let entry_type = match captures.get(1)?.as_str() {
        "d" => ArchiveEntryType::Directory,
        "l" => ArchiveEntryType::Symlink,
        _ => ArchiveEntryType::File,
    };
    Some(entry(captures.get(2)?.as_str(), entry_type, None))
}

pub fn is_zip_info_zip_listing_available() -> bool {
    quiet_success(&["which", "zipinfo"])
}

pub fn read_zip_symlink_target(
    archive_path: &str,
    entry_path: &str,
) -> Result<Option<String>, ZipListingError> {
    let stdout = run_listing(
        &["unzip", "-p", archive_path, "--", entry_path],
        "zip symlink target read failed",
    )?;
    Ok((!stdout.is_empty()).then_some(stdout))
}

pub fn list_zip_entries_with_zip_info(
    archive_path: &str,
) -> Result<Vec<ArchiveEntry>, ZipListingError> {
    if !is_zip_info_zip_listing_available() {
        return Err(listing_error(
            "zip entry listing requires zipinfo, but zipinfo is not installed".to_string(),
        ));
    }
    let stdout = run_listing(&["zipinfo", "-l", archive_path], "zip entry listing failed")?;
    let mut entries = Vec::new();
    for line in stdout
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
    {
        let Some(mut parsed) = parse_zip_info_listed_entry(line) else {
            continue;
        };
        if parsed.entry_type == ArchiveEntryType::Symlink {
            parsed.link_path = read_zip_symlink_target(archive_path, &parsed.path)?;
        }
        entries.push(parsed);
    }
    Ok(entries)
}
