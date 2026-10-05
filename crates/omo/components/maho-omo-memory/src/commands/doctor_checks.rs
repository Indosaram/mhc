//! Deterministic `/doctor` checks. Every check is read-only except the skill
//! frontmatter repair, which is the one documented auto-fix.
//! Port of `components/memory/commands/doctor-checks.ts` at pin 77f3067f1.

use std::path::{Path, PathBuf};

use memory_core::{
    locks::parse_lock_record,
    memfs::parse_memory_file,
    seeds::V1_PERSONA_SEED_SHA256,
    support::{host::hostname, sha256::sha256_hex},
};

use super::repo::run_git;
use super::tokens::estimate_system_tokens;
use super::types::{MemoryCommandDeps, MemoryCommandIdentity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

impl CheckLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

pub struct DoctorCheck {
    pub name: String,
    pub level: CheckLevel,
    pub detail: String,
}

const PERSONA_PATH: &str = "system/persona.md";

fn list_markdown(root: &Path, prefix: &str) -> Vec<String> {
    let target = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    let Ok(entries) = std::fs::read_dir(&target) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".git" || (prefix.is_empty() && name == "skills") {
            continue;
        }
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_dir() {
            files.extend(list_markdown(root, &relative));
        } else if file_type.is_file() && name.ends_with(".md") {
            files.push(relative);
        }
    }
    files
}

pub fn check_repository(identity: &MemoryCommandIdentity) -> DoctorCheck {
    let repo = &identity.identity_paths.repo;
    if !repo.join(".git").exists() {
        return DoctorCheck {
            name: "repository".to_owned(),
            level: CheckLevel::Fail,
            detail: format!("missing at {}; run /memfs init to create it", repo.display()),
        };
    }
    DoctorCheck {
        name: "repository".to_owned(),
        level: CheckLevel::Ok,
        detail: repo.display().to_string(),
    }
}

pub fn check_frontmatter(repo_dir: &Path) -> Vec<DoctorCheck> {
    let paths = list_markdown(repo_dir, "");
    let mut failures: Vec<String> = Vec::new();
    for path in &paths {
        let Ok(content) = std::fs::read_to_string(repo_dir.join(path)) else {
            continue;
        };
        if let Err(error) = parse_memory_file(&content) {
            failures.push(format!("{path} ({error})"));
        }
    }

    let frontmatter = if failures.is_empty() {
        DoctorCheck {
            name: "frontmatter".to_owned(),
            level: CheckLevel::Ok,
            detail: format!(
                "{} memory file{} valid",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" }
            ),
        }
    } else {
        DoctorCheck {
            name: "frontmatter".to_owned(),
            level: CheckLevel::Fail,
            detail: format!(
                "{} invalid file{}: {}; fix the frontmatter or remove the file",
                failures.len(),
                if failures.len() == 1 { "" } else { "s" },
                failures.join("; ")
            ),
        }
    };

    let persona = if paths.iter().any(|path| path == PERSONA_PATH) {
        DoctorCheck {
            name: "persona".to_owned(),
            level: CheckLevel::Ok,
            detail: format!("{PERSONA_PATH} present"),
        }
    } else {
        DoctorCheck {
            name: "persona".to_owned(),
            level: CheckLevel::Warn,
            detail: format!("{PERSONA_PATH} is missing; create it with /init or the memory tools"),
        }
    };

    vec![frontmatter, persona]
}

pub fn check_soul_seed(repo_dir: &Path) -> DoctorCheck {
    let Ok(content) = std::fs::read(repo_dir.join(PERSONA_PATH)) else {
        return DoctorCheck {
            name: "soul-seed".to_owned(),
            level: CheckLevel::Ok,
            detail: "no persona file to compare".to_owned(),
        };
    };
    if sha256_hex(&content) == V1_PERSONA_SEED_SHA256 {
        return DoctorCheck {
            name: "soul-seed".to_owned(),
            level: CheckLevel::Warn,
            detail: format!(
                "{PERSONA_PATH} is still the v1 seed; review it against the v2 soul seed and rewrite it with the memory tools when ready"
            ),
        };
    }
    DoctorCheck {
        name: "soul-seed".to_owned(),
        level: CheckLevel::Ok,
        detail: "persona differs from the v1 seed".to_owned(),
    }
}

pub fn check_locks(deps: &MemoryCommandDeps, locks_dir: &Path) -> DoctorCheck {
    let Ok(entries) = std::fs::read_dir(locks_dir) else {
        return DoctorCheck {
            name: "locks".to_owned(),
            level: CheckLevel::Ok,
            detail: "no lock directory yet".to_owned(),
        };
    };

    let host = hostname();
    let mut stale: Vec<String> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".lock") {
            continue;
        }
        let path = locks_dir.join(name);
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let Some(record) = parse_lock_record(&content) else {
            stale.push(format!("{} (unreadable lock record)", path.display()));
            continue;
        };
        if record.hostname != host {
            continue;
        }
        if deps.process_alive(record.pid) {
            continue;
        }
        stale.push(format!("{} (pid {} is not running)", path.display(), record.pid));
    }

    if stale.is_empty() {
        return DoctorCheck {
            name: "locks".to_owned(),
            level: CheckLevel::Ok,
            detail: "no stale locks".to_owned(),
        };
    }
    DoctorCheck {
        name: "locks".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "{} stale lock{}: {}; delete the file after confirming no run is active",
            stale.len(),
            if stale.len() == 1 { "" } else { "s" },
            stale.join("; ")
        ),
    }
}

pub fn check_worktrees(deps: &MemoryCommandDeps, identity: &MemoryCommandIdentity) -> DoctorCheck {
    let repo = &identity.identity_paths.repo;
    let worktrees = &identity.identity_paths.worktrees;
    if !repo.join(".git").exists() {
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Ok,
            detail: "no repository to inspect".to_owned(),
        };
    }

    let result = match run_git(deps, repo, &["worktree", "list", "--porcelain"]) {
        Ok(result) => result,
        Err(error) => {
            return DoctorCheck {
                name: "worktrees".to_owned(),
                level: CheckLevel::Warn,
                detail: format!("could not list worktrees: {error}"),
            };
        }
    };
    if result.code != 0 {
        let detail = if result.stderr.trim().is_empty() {
            format!("exit {}", result.code)
        } else {
            result.stderr.trim().to_owned()
        };
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Warn,
            detail: format!("could not list worktrees: {detail}"),
        };
    }

    let registered: Vec<String> = result
        .stdout
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| line.starts_with("worktree "))
        .map(|line| line["worktree ".len()..].trim().to_owned())
        .collect();

    let Ok(entries) = std::fs::read_dir(worktrees) else {
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Ok,
            detail: "no reflection worktrees".to_owned(),
        };
    };

    let orphans: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| worktrees.join(entry.file_name()).display().to_string())
        .filter(|path| !registered.contains(path))
        .collect();
    let prefix = format!("{}/", worktrees.display());
    let missing: Vec<String> = registered
        .iter()
        .filter(|path| path.starts_with(&prefix) && !Path::new(path.as_str()).exists())
        .cloned()
        .collect();

    let mut problems: Vec<String> = Vec::new();
    problems.extend(orphans.iter().map(|path| format!("{path} (not registered with git)")));
    problems.extend(
        missing
            .iter()
            .map(|path| format!("{path} (registered but missing; run git worktree prune)")),
    );
    if problems.is_empty() {
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Ok,
            detail: "no orphaned worktrees".to_owned(),
        };
    }
    DoctorCheck {
        name: "worktrees".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "{} orphan{}: {}",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" },
            problems.join("; ")
        ),
    }
}

pub fn check_abandoned_runs(reflection_dir: &Path) -> DoctorCheck {
    let runs_dir = reflection_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return DoctorCheck {
            name: "abandoned-runs".to_owned(),
            level: CheckLevel::Ok,
            detail: "no abandoned runs".to_owned(),
        };
    };
    let mut abandoned: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
                && entry.path().join("abandoned.json").exists()
        })
        .map(|entry| entry.path().display().to_string())
        .collect();
    abandoned.sort();
    if abandoned.is_empty() {
        return DoctorCheck {
            name: "abandoned-runs".to_owned(),
            level: CheckLevel::Ok,
            detail: "no abandoned runs".to_owned(),
        };
    }
    DoctorCheck {
        name: "abandoned-runs".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "{} run{} need manual disposal: {}",
            abandoned.len(),
            if abandoned.len() == 1 { "" } else { "s" },
            abandoned.join("; ")
        ),
    }
}

pub fn check_reflection_health(reflection_dir: &Path, now_ms: i64) -> DoctorCheck {
    let health = crate::worker::health::read_reflection_health(
        &reflection_dir.join("completions"),
        100,
        now_ms,
    );
    let last_success = health.last_success_at.clone().unwrap_or_else(|| "never".to_owned());
    if health.streak == 0 && health.pending_count == 0 {
        return DoctorCheck {
            name: "reflection-health".to_owned(),
            level: CheckLevel::Ok,
            detail: format!("streak 0; pending 0; last success {last_success}"),
        };
    }
    let failure = health.last_failure.as_ref();
    let hint = crate::worker::remediation::reflection_remediation(
        failure.map(|failure| failure.reason.as_str()),
        failure.and_then(|failure| failure.detail.as_deref()),
    );
    DoctorCheck {
        name: "reflection-health".to_owned(),
        level: if health.streak >= 3 { CheckLevel::Warn } else { CheckLevel::Ok },
        detail: format!(
            "streak {}; fingerprint {}; pending {}; last success {last_success}; {hint}",
            health.streak,
            if health.fingerprint.is_empty() { "none" } else { health.fingerprint.as_str() },
            health.pending_count
        ),
    }
}

pub fn check_tokens(repo_dir: &Path, warn_tokens: usize) -> DoctorCheck {
    let estimate = estimate_system_tokens(repo_dir);
    if estimate.total_tokens < warn_tokens {
        return DoctorCheck {
            name: "tokens".to_owned(),
            level: CheckLevel::Ok,
            detail: format!(
                "~{} tokens in system/ (warn at {warn_tokens})",
                estimate.total_tokens
            ),
        };
    }
    DoctorCheck {
        name: "tokens".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "~{} tokens in system/ reached the {warn_tokens} warning threshold; trim system/ and move detail into external memory",
            estimate.total_tokens
        ),
    }
}

pub fn repo_dir_of(identity: &MemoryCommandIdentity) -> PathBuf {
    identity.identity_paths.repo.clone()
}
