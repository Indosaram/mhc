//! Port of senpi `anthropic-subscription/claude-cli-account-dir.ts` (v2026.11.19-5).
//!
//! `readClaudeCliAccessToken` is only read by the usage poller, which lives in maho-core and cannot import this
//! crate; its port is in `maho_core::subscription_limits::usage_accounts`.
use crate::{claude_cli_auth_status::logout_claude_cli, claude_cli_process::ClaudeCliRun};
use maho_ai::utils::abort::AbortSignal;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Entries of a CLI-store account dir that point at the shared Claude root, so every account reads one
/// transcript tree (`projects`) and the user's own settings, memory, agents, commands, skills and output
/// styles. The credential store and everything else stay per account.
pub const CLAUDE_CLI_SHARED_LINKS: [&str; 7] = [
    "projects",
    "settings.json",
    "CLAUDE.md",
    "agents",
    "commands",
    "skills",
    "output-styles",
];

const SHARED_FILES: [&str; 2] = ["settings.json", "CLAUDE.md"];

pub fn new_account_dir_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn is_valid_account_dir_id(dir_id: &str) -> bool {
    dir_id.len() == 32
        && dir_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `<agentDir>/anthropic-subscription-claude/<dirId>` (the parent is overridable, see
/// `maho_core::config::claude_cli_account_root`); a stored id of any other shape is refused.
pub fn account_dir_for(agent_dir: &Path, dir_id: &str) -> anyhow::Result<PathBuf> {
    if !is_valid_account_dir_id(dir_id) {
        anyhow::bail!("Invalid Claude account dir id");
    }
    // SAFETY-DIVERGENCE SD-20: all lifecycle operations use the same overridable account root.
    Ok(maho_core::config::claude_cli_account_root(agent_dir).join(dir_id))
}

/// The root the accounts share: an inherited `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_cli_shared_root(env: &BTreeMap<String, String>) -> PathBuf {
    match env.get("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(inherited) => std::path::absolute(inherited).unwrap_or_else(|_| inherited.into()),
        None => maho_core::config::home_dir_path().join(".claude"),
    }
}

fn link_entry(entry: &str, target: &Path, path: &Path) -> std::io::Result<()> {
    if !SHARED_FILES.contains(&entry) {
        std::fs::create_dir_all(target)?;
        return symlink_dir(target, path);
    }
    if std::fs::symlink_metadata(target).is_err() {
        return Ok(());
    }
    match symlink_file(target, path) {
        Ok(()) => Ok(()),
        // Windows refuses file symlinks without Developer Mode; the account then keeps its own copy.
        Err(_) if cfg!(windows) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn symlink_dir(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}
#[cfg(unix)]
fn symlink_file(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}
#[cfg(windows)]
fn symlink_dir(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, path)
}
#[cfg(windows)]
fn symlink_file(target: &Path, path: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, path)
}

#[cfg(unix)]
fn restrict_to_owner(account_dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Only a dir whose mode differs is touched: an account dir already at 0700 is left exactly as it is.
    if std::fs::metadata(account_dir)?.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(account_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn restrict_to_owner(_account_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn create_private_account_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if !std::fs::symlink_metadata(path)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "Claude account path is not a directory",
        ));
    }
    restrict_to_owner(path)
}

/// `path` with its parent's links resolved, so `~/.claude/projects` and the same entry reached through a linked home
/// compare equal even while the entry itself is missing (a dangling `CLAUDE.md` link).
fn resolved_location(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or(absolute.clone()),
        _ => absolute,
    }
}

/// Creates the account dir (0700) and creates or repairs its links to the shared root. A link pointing
/// elsewhere is replaced; a real file or dir in a link's place is account data and is left alone.
pub fn ensure_account_dir(
    agent_dir: &Path,
    dir_id: &str,
    shared_root: &Path,
) -> anyhow::Result<PathBuf> {
    let account_dir = account_dir_for(agent_dir, dir_id)?;
    create_private_account_dir(&account_dir)?;
    for entry in CLAUDE_CLI_SHARED_LINKS {
        let path = account_dir.join(entry);
        let target = shared_root.join(entry);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let linked = std::fs::read_link(&path)?;
                if resolved_location(&account_dir.join(&linked)) == resolved_location(&target) {
                    continue;
                }
                std::fs::remove_file(&path)?;
            }
            Ok(_) => continue,
            Err(_) => {}
        }
        link_entry(entry, &target, &path)?;
    }
    Ok(account_dir)
}

pub struct RemoveAccountDirOptions<'a> {
    pub agent_dir: &'a Path,
    pub dir_id: &'a str,
    pub executable: Option<&'a Path>,
    pub parent_env: &'a BTreeMap<String, String>,
    pub signal: Option<&'a AbortSignal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveAccountDirOutcome {
    Removed,
    RemovedWithoutSignOut,
    NotAnAccountDir,
}

/// Sign out against the exact (possibly SD-20-overridden) path, then remove only that directory.
/// A missing directory still needs a sign-out: the credential store is keyed by the path.
pub async fn remove_account_dir(
    options: RemoveAccountDirOptions<'_>,
) -> anyhow::Result<RemoveAccountDirOutcome> {
    let account_dir = account_dir_for(options.agent_dir, options.dir_id)?;
    if let Some(signal) = options.signal {
        signal.throw_if_aborted()?;
    }
    let before = match std::fs::symlink_metadata(&account_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_account_dir(&account_dir)?;
            std::fs::symlink_metadata(&account_dir)?
        }
        metadata => metadata?,
    };
    if !before.is_dir() {
        return Ok(RemoveAccountDirOutcome::NotAnAccountDir);
    }
    let signed_out = match options.executable {
        Some(executable) => {
            logout_claude_cli(
                &ClaudeCliRun {
                    executable,
                    config_dir: account_dir.clone(),
                    parent_env: options.parent_env,
                },
                None,
                options.signal,
            )
            .await
        }
        None => false,
    };
    finish_account_dir_removal(account_dir, before, signed_out, options.signal)
}

fn finish_account_dir_removal(
    account_dir: PathBuf,
    before: std::fs::Metadata,
    signed_out: bool,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<RemoveAccountDirOutcome> {
    if !signed_out && let Some(signal) = signal {
        signal.throw_if_aborted()?;
    }
    match std::fs::symlink_metadata(&account_dir) {
        Ok(after) if !after.is_dir() => return Ok(RemoveAccountDirOutcome::NotAnAccountDir),
        #[cfg(unix)]
        Ok(after) => {
            use std::os::unix::fs::MetadataExt;
            if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
                return Ok(RemoveAccountDirOutcome::NotAnAccountDir);
            }
        }
        #[cfg(not(unix))]
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // Unlike manually walking shared paths, remove_dir_all never follows symlinks.
    match std::fs::remove_dir_all(account_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(if signed_out {
        RemoveAccountDirOutcome::Removed
    } else {
        RemoveAccountDirOutcome::RemovedWithoutSignOut
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const DIR_ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn completed_logout_is_committed_despite_a_late_abort() {
        let temp = tempfile::tempdir().expect("temp");
        let completed = temp.path().join("completed");
        std::fs::create_dir(&completed).expect("completed account directory");
        let before = std::fs::symlink_metadata(&completed).expect("metadata");
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        assert_eq!(
            finish_account_dir_removal(completed.clone(), before, true, Some(&controller.signal()))
                .expect("completed logout is committed"),
            RemoveAccountDirOutcome::Removed
        );
        assert!(!completed.exists());

        let pending = temp.path().join("pending");
        std::fs::create_dir(&pending).expect("pending account directory");
        let before = std::fs::symlink_metadata(&pending).expect("metadata");
        assert!(
            finish_account_dir_removal(pending.clone(), before, false, Some(&controller.signal()))
                .is_err()
        );
        assert!(pending.exists());
    }

    #[test]
    fn dir_ids_are_32_lowercase_hex() {
        assert!(is_valid_account_dir_id(DIR_ID));
        assert!(!is_valid_account_dir_id("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_valid_account_dir_id("../escape"));
        assert!(account_dir_for(Path::new("/agent"), "nope").is_err());
    }

    #[test]
    fn ensure_creates_links_repairs_a_moved_one_and_keeps_account_data() {
        let temp = tempfile::tempdir().expect("temp");
        let agent = temp.path().join("agent");
        let shared = temp.path().join("claude");
        assert!(
            account_dir_for(&agent, DIR_ID)
                .unwrap()
                .starts_with(temp.path())
        );
        std::fs::create_dir_all(&shared).expect("shared");
        std::fs::write(shared.join("settings.json"), "{}").expect("settings");
        let dir = ensure_account_dir(&agent, DIR_ID, &shared).expect("ensure");
        assert_eq!(
            dir,
            agent.join("anthropic-subscription-claude").join(DIR_ID)
        );
        assert_eq!(
            std::fs::read_link(dir.join("projects")).expect("projects link"),
            shared.join("projects")
        );
        assert_eq!(
            std::fs::read_link(dir.join("settings.json")).expect("settings link"),
            shared.join("settings.json")
        );
        // CLAUDE.md is absent from the shared root, so no link is made for it.
        assert!(std::fs::symlink_metadata(dir.join("CLAUDE.md")).is_err());
        std::fs::remove_file(dir.join("agents")).expect("unlink");
        std::os::unix::fs::symlink(temp.path(), dir.join("agents")).expect("foreign link");
        std::fs::remove_file(dir.join("skills")).expect("unlink");
        std::fs::create_dir(dir.join("skills")).expect("account data");
        ensure_account_dir(&agent, DIR_ID, &shared).expect("repair");
        assert_eq!(
            std::fs::read_link(dir.join("agents")).expect("agents link"),
            shared.join("agents")
        );
        assert!(dir.join("skills").is_dir());
        // A shared root reached through a linked home is the same root: its links are kept as they are.
        let linked_home = temp.path().join("home");
        std::fs::create_dir(&linked_home).expect("home");
        std::os::unix::fs::symlink(&shared, linked_home.join(".claude")).expect("linked root");
        let before = std::fs::symlink_metadata(dir.join("projects"))
            .expect("projects")
            .modified()
            .expect("mtime");
        ensure_account_dir(&agent, DIR_ID, &linked_home.join(".claude")).expect("same root");
        assert_eq!(
            std::fs::read_link(dir.join("projects")).expect("projects link"),
            shared.join("projects")
        );
        assert_eq!(
            std::fs::symlink_metadata(dir.join("projects"))
                .expect("projects")
                .modified()
                .expect("mtime"),
            before
        );
        assert!(
            !std::fs::symlink_metadata(dir.join("skills"))
                .expect("skills")
                .file_type()
                .is_symlink()
        );
    }
}
