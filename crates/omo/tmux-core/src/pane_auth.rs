//! Private one-use password channel for pane shells (`pane-auth.ts`).
//!
//! The password never reaches tmux argv: the server writes it to a `0600` file inside a `0700`
//! directory, passes only the file path (`-e OPENCODE_SERVER_PASSWORD_FILE=...`), and the pane
//! shell sources and unlinks it before launching the child.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::env_source::{EnvSource, ProcessEnv, is_set};

/// The server owns the directory; pane shells consume and unlink individual files. Dropping the
/// guard removes the directory with every file still in it (upstream removes it on cleanup or
/// process exit).
#[derive(Debug)]
pub struct PaneAuthDirectory {
    path: PathBuf,
}

impl PaneAuthDirectory {
    /// The `OPENCODE_SERVER_PASSWORD_DIR` value this directory stands for.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PaneAuthDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `mkdtemp(<tmp>/omo-run-auth-)` with mode `0700`.
///
/// SAFETY-DIVERGENCE(omo pane-auth): upstream also exports `OPENCODE_SERVER_PASSWORD_DIR` into
/// `process.env`; this crate forbids `unsafe` (`std::env::set_var`), so callers pass the returned
/// path through their injected environment instead.
pub fn create_pane_auth_directory() -> std::io::Result<PaneAuthDirectory> {
    let path = std::env::temp_dir().join(format!("omo-run-auth-{}", uuid::Uuid::new_v4()));
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    Ok(PaneAuthDirectory { path })
}

/// Lazily created directory for a standalone, user-password-configured server that set no
/// `OPENCODE_SERVER_PASSWORD_DIR` (upstream `buildPaneAuthEnvironmentArgs` creates one and removes
/// it at process exit).
///
/// SAFETY-DIVERGENCE(omo pane-auth): Rust has no safe exit hook and a `static` is never dropped,
/// so the directory is owned by the injected dependencies (`TmuxDeps::pane_auth`, team-core
/// `TeamLayoutDeps::pane_auth`) instead of the process. Dropping the last owner removes the
/// directory together with every password file a pane has not consumed yet, so the owner must
/// outlive the panes it spawns (upstream: the `omo run` server's lifetime).
#[derive(Debug, Default)]
pub struct PaneAuthFallbackDirectory {
    directory: Mutex<Option<PaneAuthDirectory>>,
}

impl PaneAuthFallbackDirectory {
    /// The directory, created on first use.
    fn path(&self) -> std::io::Result<PathBuf> {
        let mut slot = self
            .directory
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(directory) = slot.as_ref() {
            return Ok(directory.path().to_path_buf());
        }
        let directory = create_pane_auth_directory()?;
        let path = directory.path().to_path_buf();
        *slot = Some(directory);
        Ok(path)
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// `-e OPENCODE_SERVER_PASSWORD_FILE=<one-use file>` (plus username when set); empty when no
/// password is set. Without `OPENCODE_SERVER_PASSWORD_DIR` the file goes into `fallback`. Fails
/// closed: an unwritable password file is an error, never a fallback to the password in argv.
pub fn build_pane_auth_environment_args_from(
    environment: &dyn EnvSource,
    fallback: &PaneAuthFallbackDirectory,
) -> std::io::Result<Vec<String>> {
    let password = environment.var("OPENCODE_SERVER_PASSWORD");
    let Some(password) = password.filter(|password| !password.is_empty()) else {
        return Ok(Vec::new());
    };
    let directory = match environment
        .var("OPENCODE_SERVER_PASSWORD_DIR")
        .filter(|directory| !directory.is_empty())
    {
        Some(directory) => PathBuf::from(directory),
        None => fallback.path()?,
    };
    let file = directory.join(format!("{}.sh", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(&file)?;
    handle.write_all(
        format!(
            "export OPENCODE_SERVER_PASSWORD={}\n",
            shell_single_quote(&password)
        )
        .as_bytes(),
    )?;
    let mut args = vec![
        "-e".to_owned(),
        format!("OPENCODE_SERVER_PASSWORD_FILE={}", file.to_string_lossy()),
    ];
    if let Some(username) = environment.var("OPENCODE_SERVER_USERNAME") {
        args.push("-e".to_owned());
        args.push(format!("OPENCODE_SERVER_USERNAME={username}"));
    }
    Ok(args)
}

/// [`build_pane_auth_environment_args_from`] over the process environment.
pub fn build_pane_auth_environment_args(
    fallback: &PaneAuthFallbackDirectory,
) -> std::io::Result<Vec<String>> {
    build_pane_auth_environment_args_from(&ProcessEnv, fallback)
}

/// Read once, unlink before launching the child, and fail closed if either fails.
#[must_use]
pub fn with_pane_auth_from(command: &str, environment: &dyn EnvSource) -> String {
    if !is_set(environment.var("OPENCODE_SERVER_PASSWORD").as_deref()) {
        return command.to_owned();
    }
    format!(
        ". \"$OPENCODE_SERVER_PASSWORD_FILE\" && rm -f -- \"$OPENCODE_SERVER_PASSWORD_FILE\" && {command}"
    )
}

/// [`with_pane_auth_from`] over the process environment.
#[must_use]
pub fn with_pane_auth(command: &str) -> String {
    with_pane_auth_from(command, &ProcessEnv)
}
