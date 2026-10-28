//! Bank files and project scoping.
//!
//! Port of mnemopi `core/banks.ts` (`BankManager.getBankDbPath`, name validation, listing) and of
//! the coding-agent bank derivation in oh-my-pi `packages/coding-agent/src/mnemopi/config.ts`
//! (`computeMnemopiBankScope`, `projectBank`, `projectBankSegment`, `sanitizeBankName`,
//! `limitBankName`). The project hash is SHA-256 based (upstream uses Bun's hash), so bank names
//! are stable for mhc but not identical to oh-my-pi's.
use crate::store::sha256_hex16;
use std::path::{Path, PathBuf};

/// File name of every bank database.
pub const DB_FILENAME: &str = "mnemopi.db";
/// The global bank.
pub const DEFAULT_BANK: &str = "default";

/// How sessions map to banks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scoping {
    /// One shared bank for everything.
    Global,
    /// One bank per project; nothing shared.
    PerProject,
    /// Writes go to the project bank; recall also reads the global bank (the default).
    PerProjectTagged,
}

impl Scoping {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "global" => Some(Self::Global),
            "per-project" => Some(Self::PerProject),
            "per-project-tagged" => Some(Self::PerProjectTagged),
            _ => None,
        }
    }
}

/// The bank a session writes to and the banks it recalls from (write bank first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankScope {
    pub bank: String,
    pub recall_banks: Vec<String>,
}

/// Bank directory layout under one data directory.
#[derive(Clone, Debug)]
pub struct Banks {
    pub data_dir: PathBuf,
}

impl Banks {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// `<data>/mnemopi.db` for the default bank, `<data>/banks/<name>/mnemopi.db` otherwise.
    pub fn db_path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty() || name == DEFAULT_BANK {
            return Ok(self.data_dir.join(DB_FILENAME));
        }
        validate_name(name)?;
        Ok(self.data_dir.join("banks").join(name).join(DB_FILENAME))
    }

    /// `default` plus every bank directory, sorted.
    pub fn list(&self) -> Vec<String> {
        let mut banks = vec![DEFAULT_BANK.to_owned()];
        if let Ok(entries) = std::fs::read_dir(self.data_dir.join("banks")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir() && name != DEFAULT_BANK && validate_name(&name).is_ok() {
                    banks.push(name);
                }
            }
        }
        banks.sort();
        banks
    }
}

/// Bank names are 1-64 characters of `[A-Za-z0-9_-]`.
pub fn validate_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid memory bank name '{name}' (use 1-64 letters, digits, '_' or '-')"
        ))
    }
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

fn short_hash(text: &str) -> String {
    let hex = sha256_hex16(text);
    base36(u64::from_str_radix(&hex, 16).unwrap_or_default())
}

fn limit_name(name: &str) -> String {
    if name.len() <= 64 {
        return name.to_owned();
    }
    let hash = short_hash(name);
    let prefix_len = 63usize.saturating_sub(hash.len()).max(1);
    let prefix: String = name.chars().take(prefix_len).collect();
    let prefix = prefix.trim_end_matches('-');
    let prefix = if prefix.is_empty() { "bank" } else { prefix };
    format!("{prefix}-{hash}")
}

/// Runs of characters outside `[A-Za-z0-9_-]` become `-`, outer dashes are trimmed, and the
/// result is clamped to 64 characters; `None` when nothing is left.
pub fn sanitize_name(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    (!trimmed.is_empty()).then(|| limit_name(trimmed))
}

/// `<sanitized basename>-<hash of the absolute path>`, derived from `cwd` alone.
pub fn project_bank(cwd: &Path) -> String {
    let root = std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let base = root
        .file_name()
        .and_then(|name| sanitize_name(&name.to_string_lossy()))
        .unwrap_or_else(|| DEFAULT_BANK.to_owned());
    limit_name(&format!("{base}-{}", short_hash(&root.to_string_lossy())))
}

/// Write and recall banks for a session in `cwd`.
pub fn bank_scope(scoping: Scoping, cwd: &Path) -> BankScope {
    match scoping {
        Scoping::Global => BankScope {
            bank: DEFAULT_BANK.into(),
            recall_banks: vec![DEFAULT_BANK.into()],
        },
        Scoping::PerProject => {
            let project = project_bank(cwd);
            BankScope {
                bank: project.clone(),
                recall_banks: vec![project],
            }
        }
        Scoping::PerProjectTagged => {
            let project = project_bank(cwd);
            BankScope {
                bank: project.clone(),
                recall_banks: vec![project, DEFAULT_BANK.into()],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_bank_layout() {
        let banks = Banks::new("/data");
        assert_eq!(
            banks.db_path("default").expect("path"),
            PathBuf::from("/data/mnemopi.db")
        );
        assert_eq!(
            banks.db_path("p1").expect("path"),
            PathBuf::from("/data/banks/p1/mnemopi.db")
        );
        assert!(banks.db_path("../x").is_err());
    }

    #[test]
    fn names_are_sanitized_and_limited() {
        assert_eq!(sanitize_name("  My Project!! "), Some("My-Project".into()));
        assert_eq!(sanitize_name("***"), None);
        let long = sanitize_name(&"a".repeat(100)).expect("name");
        assert!(long.len() <= 64);
        assert!(validate_name(&long).is_ok());
    }

    #[test]
    fn project_banks_are_stable_and_distinct() {
        let a = project_bank(Path::new("/work/app"));
        assert_eq!(a, project_bank(Path::new("/work/app")));
        assert!(a.starts_with("app-"));
        assert_ne!(a, project_bank(Path::new("/other/app")));
        assert!(validate_name(&a).is_ok());
    }

    #[test]
    fn tagged_scope_recalls_project_then_global() {
        let scope = bank_scope(Scoping::PerProjectTagged, Path::new("/work/app"));
        assert_eq!(scope.recall_banks.len(), 2);
        assert_eq!(scope.recall_banks[1], DEFAULT_BANK);
        assert_eq!(
            bank_scope(Scoping::Global, Path::new("/x")).bank,
            DEFAULT_BANK
        );
    }
}
