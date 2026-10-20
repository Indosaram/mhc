//! Push-only mirror of a memory repository.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::redact::redact_url;
use crate::git::GitMemoryRepo;
use crate::git::config_lock::with_serialized_git_config_mutation;
use crate::git::errors::GitError;
use crate::git::exec::{GitExec, GitExecOptions, GitExecResult};

/// Repo-local git config key holding the mirror URL.
pub const CONFIG_KEY: &str = "omo.memoryRepository.url";

/// Push log file name relative to the repository's git directory.
pub const LOG_NAME: &str = "memory-repository-push.log";

/// Set to 1 to run the post-commit push in the foreground.
pub const SYNC_PUSH_ENV: &str = "OMO_MEMORY_PUSH_SYNC";

const BRANCH: &str = "main";
const INITIAL_PUSH_TIMEOUT_MS: u64 = 10_000;
const PUSH_TIMEOUT_MS: u64 = 60_000;
const QUERY_TIMEOUT_MS: u64 = 10_000;
const STATUS_LOG_LINES: usize = 20;
const STATUS_LOG_TAIL_BYTES: u64 = 64 * 1024;

/// Returns the complete lines in the last `STATUS_LOG_TAIL_BYTES` of a log larger than that window,
/// or `None` when the caller should read the whole file (small file, read error, too few lines).
fn read_tail_window(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len <= STATUS_LOG_TAIL_BYTES {
        return None;
    }
    file.seek(SeekFrom::Start(len - STATUS_LOG_TAIL_BYTES))
        .ok()?;
    let mut bytes = Vec::with_capacity(STATUS_LOG_TAIL_BYTES as usize);
    file.read_to_end(&mut bytes).ok()?;
    // Drop the partial first line so a split line (or split UTF-8 sequence) is never reported.
    let first_newline = bytes.iter().position(|&b| b == b'\n')?;
    let tail = String::from_utf8(bytes.split_off(first_newline + 1)).ok()?;
    let complete_lines = tail.lines().filter(|l| !l.trim().is_empty()).count();
    (complete_lines >= STATUS_LOG_LINES).then_some(tail)
}

/// Error type for synchronization operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncError {
    pub message: String,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SyncError {}

/// Outcome of a push attempt to a mirror remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorPushResult {
    pub pushed: bool,
    pub detail: String,
}

/// Outcome of configuring a mirror remote, including initial push attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorSetResult {
    pub url: String,
    pub pushed: bool,
    pub detail: String,
}

/// Status snapshot of a mirror configuration and recent push logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_url: Option<String>,
    pub last_log_lines: Vec<String>,
    pub ahead_count: usize,
}

/// Strip surrounding whitespace and trailing slashes from a URL.
pub fn normalize_url(url: &str) -> String {
    let trimmed = url.trim();
    let stripped = trimmed.trim_end_matches('/');
    if stripped.is_empty() {
        trimmed.to_string()
    } else {
        stripped.to_string()
    }
}

/// Absolute path of the push log for a repository working directory.
pub fn mirror_log_path(repo_dir: &Path) -> PathBuf {
    repo_dir.join(".git").join(LOG_NAME)
}

/// The post-commit snippet the hook installer embeds.
pub fn get_post_commit_hook_script() -> &'static str {
    "#!/bin/sh\n\
# Push memory commits to the configured memory-repository mirror.\n\
# Installed by omo memory-core. Do not edit by hand - regenerated on startup.\n\
url=$(git config --local --get omo.memoryRepository.url 2>/dev/null)\n\
[ -z \"$url\" ] && exit 0\n\
\n\
branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || exit 0\n\
[ -z \"$branch\" ] && exit 0\n\
[ \"$branch\" != \"main\" ] && exit 0\n\
\n\
log=\"$(git rev-parse --git-dir)/memory-repository-push.log\"\n\
\n\
push_to_mirror() {\n\
  {\n\
    printf '\\n--- %s %s on %s ---\\n' \"$(date '+%Y-%m-%dT%H:%M:%S')\" \"$(git rev-parse --short HEAD)\" \"$branch\"\n\
    git push --quiet \"$url\" \"$branch:$branch\" 2>&1\n\
    echo \"exit=$?\"\n\
  } >> \"$log\" 2>&1\n\
}\n\
\n\
if [ \"$OMO_MEMORY_PUSH_SYNC\" = \"1\" ]; then\n\
  push_to_mirror\n\
  exit 0\n\
fi\n\
\n\
# Detach the background push from git's stdout/stderr: a child that keeps them open makes the\n\
# commit wait for it (omo #10199). A bare exec redirects for good; a redirect on the function\n\
# call would make dash keep saved copies of the originals open for the whole push.\n\
( exec </dev/null >/dev/null 2>&1; push_to_mirror ) &\n\
exit 0\n"
}

fn describe(result: &GitExecResult) -> String {
    let stderr = result.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = result.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    format!("git exited with code {}", result.code)
}

/// Push-only mirror manager for a memory repository.
pub struct MirrorSync<'a> {
    repo: &'a GitMemoryRepo,
    exec: Option<&'a dyn GitExec>,
}

impl<'a> MirrorSync<'a> {
    /// Creates a new MirrorSync instance bound to a repository.
    pub fn new(repo: &'a GitMemoryRepo) -> Self {
        Self { repo, exec: None }
    }

    /// Creates a new MirrorSync instance with a custom GitExec runner.
    pub fn with_exec(repo: &'a GitMemoryRepo, exec: &'a dyn GitExec) -> Self {
        Self {
            repo,
            exec: Some(exec),
        }
    }

    /// Configure the mirror and attempt one best-effort initial push.
    pub fn set(&self, url: &str) -> Result<MirrorSetResult, SyncError> {
        let normalized = normalize_url(url);
        if normalized.is_empty() {
            return Err(SyncError {
                message: "Memory repository URL must not be empty".to_string(),
            });
        }

        self.repo
            .config_set(CONFIG_KEY, &normalized)
            .map_err(|error| SyncError {
                message: error.to_string(),
            })?;

        let push = self.push(&normalized, INITIAL_PUSH_TIMEOUT_MS);
        Ok(MirrorSetResult {
            url: normalized,
            pushed: push.pushed,
            detail: push.detail,
        })
    }

    /// Remove the mirror configuration. Future commits stop pushing and stop logging.
    ///
    /// The `--unset-all` mutation runs inside `with_serialized_git_config_mutation`, the same
    /// per-repository queue `GitMemoryRepo::config_set` uses (pin `mirror.ts:93`), so a mirror
    /// unset can never interleave with a concurrent git-config mutation.
    pub fn unset(&self) -> Result<(), SyncError> {
        with_serialized_git_config_mutation(&self.repo.dir, || {
            let result = self.run(
                &["config", "--local", "--unset-all", CONFIG_KEY],
                QUERY_TIMEOUT_MS,
            );
            // Exit code 5 is "key was not there", which is the desired end state.
            if result.code != 0 && result.code != 5 {
                return Err(GitError::Other(describe(&result)));
            }
            Ok(())
        })
        .map_err(|error| SyncError {
            message: error.to_string(),
        })
    }

    /// Read the current mirror status, ahead count, and redacted log tail.
    pub fn status(&self) -> MirrorStatus {
        let url = self.configured_url();
        let last_log_lines = self.read_log_tail();
        let Some(url) = url else {
            return MirrorStatus {
                url: None,
                redacted_url: None,
                last_log_lines,
                ahead_count: 0,
            };
        };

        let redacted_url = Some(redact_url(&url));
        let ahead_count = self.ahead_count(&url);

        MirrorStatus {
            url: Some(url),
            redacted_url,
            last_log_lines,
            ahead_count,
        }
    }

    /// One-shot foreground push of main:main. Never throws on push failure.
    pub fn push_now(&self) -> MirrorPushResult {
        let url = self.configured_url();
        let Some(url) = url else {
            return MirrorPushResult {
                pushed: false,
                detail: "No memory repository configured. Set one first.".to_string(),
            };
        };

        self.push(&url, PUSH_TIMEOUT_MS)
    }

    fn configured_url(&self) -> Option<String> {
        let raw = self.repo.config_get(CONFIG_KEY).ok().flatten();
        let normalized = normalize_url(raw.as_deref().unwrap_or(""));
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    }

    fn push(&self, url: &str, timeout_ms: u64) -> MirrorPushResult {
        let branch_ref = format!("{BRANCH}:{BRANCH}");
        let result = self.run(&["push", "--quiet", url, &branch_ref], timeout_ms);
        if result.code == 0 {
            return MirrorPushResult {
                pushed: true,
                detail: String::new(),
            };
        }
        MirrorPushResult {
            pushed: false,
            detail: redact_url(&describe(&result)),
        }
    }

    fn ahead_count(&self, url: &str) -> usize {
        let branch_head = format!("refs/heads/{BRANCH}");
        let remote = self.run(&["ls-remote", url, &branch_head], QUERY_TIMEOUT_MS);
        let remote_sha = if remote.code == 0 {
            remote.stdout.split_whitespace().next().unwrap_or("")
        } else {
            ""
        };
        let range = if !remote_sha.is_empty() {
            format!("{remote_sha}..{BRANCH}")
        } else {
            BRANCH.to_string()
        };
        let counted = self.run(&["rev-list", "--count", &range], QUERY_TIMEOUT_MS);
        if counted.code != 0 {
            return 0;
        }
        counted.stdout.trim().parse::<usize>().unwrap_or(0)
    }

    fn read_log_tail(&self) -> Vec<String> {
        let path = mirror_log_path(&self.repo.dir);
        // The post-commit hook appends forever; read only the trailing window, falling back to the
        // whole file when the window holds fewer than STATUS_LOG_LINES complete lines.
        let raw = match read_tail_window(&path) {
            Some(raw) => raw,
            None => match fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(_) => return Vec::new(),
            },
        };

        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        let take_start = lines.len().saturating_sub(STATUS_LOG_LINES);
        lines[take_start..].iter().map(|l| redact_url(l)).collect()
    }

    fn run(&self, argv: &[&str], timeout_ms: u64) -> GitExecResult {
        let string_argv: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        let options = GitExecOptions {
            cwd: self.repo.dir.clone(),
            timeout_ms,
            ..GitExecOptions::default()
        };

        if let Some(exec) = self.exec {
            return exec
                .run(&string_argv, &options)
                .unwrap_or_else(|error| GitExecResult {
                    code: 1,
                    stdout: String::new(),
                    stderr: error.to_string(),
                });
        }

        self.repo
            .exec()
            .run(&string_argv, &options)
            .unwrap_or_else(|error| GitExecResult {
                code: 1,
                stdout: String::new(),
                stderr: error.to_string(),
            })
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "linux"))]
mod hook_detachment_tests {
    use std::process::{Command, Stdio};

    #[test]
    fn background_push_when_hook_has_pipes_then_closes_inherited_output_descriptors() {
        // Given: piped hook output and a push probe inspecting the shell's saved descriptors.
        let temp = tempfile::tempdir().expect("temporary repository");
        let script = super::get_post_commit_hook_script();
        // Wait only in the test harness so log assertions observe the completed background job.
        let script = script.strip_suffix("exit 0\n").expect("hook exit");
        let harness = format!(
            "hook_stdout=$(readlink /proc/$$/fd/1)
hook_stderr=$(readlink /proc/$$/fd/2)
git() {{
  case \"$1\" in
    config) printf 'mirror\\n' ;;
    symbolic-ref) printf 'main\\n' ;;
    rev-parse) printf '.\\n' ;;
    push)
      read -r push_pid rest < /proc/self/stat
      for fd in /proc/\"$push_pid\"/fd/*; do
        target=$(readlink \"$fd\") || continue
        if [ \"$target\" = \"$hook_stdout\" ] || [ \"$target\" = \"$hook_stderr\" ]; then
          printf 'inherited-output\\n'
        fi
      done
      printf 'push-stdout\\n'
      printf 'push-stderr\\n' >&2
      ;;
    *) return 1 ;;
  esac
}}
{script}wait\n"
        );
        let child = Command::new("sh")
            .arg("-c")
            .arg(harness)
            .current_dir(temp.path())
            .env_remove(super::SYNC_PUSH_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start hook");
        // When: the generated hook runs its background push to completion.
        let output = child.wait_with_output().expect("finish hook");

        // Then: no original output pipe is retained, and push output still reaches the log.
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let log = std::fs::read_to_string(temp.path().join(super::LOG_NAME)).expect("push log");
        assert!(!log.contains("inherited-output\n"));
        assert!(log.contains("push-stdout\n"));
        assert!(log.contains("push-stderr\n"));
        assert!(log.contains("exit=0\n"));
    }
}
