//! Port of senpi `anthropic-subscription/claude-cli-account-dir.ts` (v2026.11.19-5).
//!
//! `readClaudeCliAccessToken` is only read by the usage poller, which lives in maho-core and cannot import this
//! crate; its port is in `maho_core::subscription_limits::usage_accounts`. `newAccountDirId` and `removeAccountDir`
//! belong to the claude CLI `/login` and account removal, which this port does not carry.
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
    if !account_dir.is_dir() {
        std::fs::create_dir_all(&account_dir)?;
    }
    restrict_to_owner(&account_dir)?;
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const DIR_ID: &str = "0123456789abcdef0123456789abcdef";

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
