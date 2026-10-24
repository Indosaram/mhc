//! SAFETY-DIVERGENCE(senpi tools/bash.ts): bash runs unconfined; opt-in bwrap/sandbox-exec write
//! confinement via `bashSandbox` (default off).
//!
//! Opt-in OS write-confinement sandbox for the bash tool (`bashSandbox`).
//!
//! Scope: WRITE confinement only. Reads and network access are NOT restricted. Commands may write
//! under the working directory, the extra `writable` roots and a private per-session temp dir; the
//! host's shared temp roots are never writable.
//! - Linux: `bwrap` (bubblewrap). `/tmp` is a fresh private tmpfs: writes there succeed but are
//!   invisible to the host and discarded on exit.
//! - macOS: `sandbox-exec` with a deny-write profile. There is no `/tmp` overlay, so `/tmp` and
//!   `/private/var/folders` are not writable; tools use `TMPDIR=<private_tmp>`.
//! - Windows / tool missing: `Auto` runs unwrapped with a warning, `Required` fails closed.
use crate::{bash::BashSpawnContext, definition::ToolError};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BashSandboxMode {
    #[default]
    Off,
    Auto,
    Required,
}

impl BashSandboxMode {
    /// Parses a `bashSandbox` setting value; anything unrecognised is `Off`.
    pub fn parse(value: &str) -> Self {
        match value {
            "auto" => Self::Auto,
            "required" => Self::Required,
            _ => Self::Off,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Platform {
    Linux,
    MacOs,
    Other,
}

fn current_platform() -> Platform {
    if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Other
    }
}

/// Finds an executable on `PATH`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Creates `<agent_dir>/tmp/bash-<session-id>` (0700) plus the writable agent subdirectories, so
/// the sandbox can bind them. Returns `(private_tmp, writable)`.
pub fn prepare_dirs(
    agent_dir: &Path,
    session_id: &str,
) -> Result<(PathBuf, Vec<PathBuf>), ToolError> {
    let id: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let private_tmp = agent_dir.join("tmp").join(format!("bash-{id}"));
    std::fs::create_dir_all(private_tmp.parent().unwrap_or(agent_dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        match std::fs::DirBuilder::new().mode(0o700).create(&private_tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        std::fs::set_permissions(&private_tmp, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&private_tmp)?;
    let writable = vec![agent_dir.join("sessions"), agent_dir.join("cache")];
    for dir in &writable {
        std::fs::create_dir_all(dir)?;
    }
    Ok((private_tmp, writable))
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Escapes a string for a sandbox-exec (TinyScheme) string literal.
fn profile_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Pure: the shell command that runs `command` under bwrap. All paths must already be canonical.
pub fn linux_command(
    bwrap: &Path,
    shell: &Path,
    cwd: &Path,
    private_tmp: &Path,
    writable: &[PathBuf],
    command: &str,
) -> String {
    let (cwd, tmp) = (sh_quote(&path_text(cwd)), sh_quote(&path_text(private_tmp)));
    let mut text = format!(
        "{} --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --bind {tmp} {tmp} --setenv TMPDIR {tmp} --bind {cwd} {cwd}",
        sh_quote(&path_text(bwrap))
    );
    for path in writable {
        let path = sh_quote(&path_text(path));
        text.push_str(&format!(" --bind {path} {path}"));
    }
    text.push_str(&format!(
        " --chdir {cwd} --die-with-parent --new-session -- {} -c {}",
        sh_quote(&path_text(shell)),
        sh_quote(command)
    ));
    text
}

/// Pure: the sandbox-exec profile. All paths must already be canonical.
pub fn macos_profile(cwd: &Path, private_tmp: &Path, writable: &[PathBuf]) -> String {
    let mut profile = format!(
        "(version 1)(allow default)(deny file-write*)(allow file-write* (subpath \"{}\")",
        profile_escape(&path_text(cwd))
    );
    for path in writable {
        profile.push_str(&format!(
            " (subpath \"{}\")",
            profile_escape(&path_text(path))
        ));
    }
    profile.push_str(&format!(
        " (subpath \"{}\") (literal \"/dev/null\") (literal \"/dev/tty\"))",
        profile_escape(&path_text(private_tmp))
    ));
    profile
}

/// Pure: the shell command that runs `command` under sandbox-exec.
pub fn macos_command(sandbox_exec: &Path, shell: &Path, profile: &str, command: &str) -> String {
    format!(
        "{} -p {} {} -c {}",
        sh_quote(&path_text(sandbox_exec)),
        sh_quote(profile),
        sh_quote(&path_text(shell)),
        sh_quote(command)
    )
}

fn wrap_for(
    platform: Platform,
    mut ctx: BashSpawnContext,
    mode: BashSandboxMode,
    private_tmp: &Path,
    writable: &[PathBuf],
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<(BashSpawnContext, Option<String>), ToolError> {
    if mode == BashSandboxMode::Off {
        return Ok((ctx, None));
    }
    let (tool, shell) = match platform {
        Platform::Linux => ("bwrap", Some(which("bwrap"))),
        Platform::MacOs => ("sandbox-exec", Some(which("sandbox-exec"))),
        Platform::Other => ("sandbox-exec", None),
    };
    let found = shell.flatten();
    let Some(binary) = found else {
        let reason = if platform == Platform::Other {
            "OS sandbox is not supported on this platform".to_owned()
        } else {
            format!("{tool} was not found")
        };
        return if mode == BashSandboxMode::Required {
            Err(ToolError::Message(format!(
                "bashSandbox is \"required\" but {tool} is not available on this system"
            )))
        } else {
            Ok((ctx, Some(format!("bash sandbox unavailable: {reason}"))))
        };
    };
    let shell = which("bash").unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let cwd = canonical(&ctx.cwd);
    let tmp = canonical(private_tmp);
    let writable: Vec<PathBuf> = writable.iter().map(|path| canonical(path)).collect();
    ctx.command = match platform {
        Platform::Linux => linux_command(&binary, &shell, &cwd, &tmp, &writable, &ctx.command),
        _ => {
            let profile = macos_profile(&cwd, &tmp, &writable);
            macos_command(&binary, &shell, &profile, &ctx.command)
        }
    };
    ctx.env.insert("TMPDIR".into(), path_text(&tmp));
    Ok((ctx, None))
}

/// Like [`wrap`] but also returns the one-line warning when `Auto` fell back to running unwrapped.
pub fn wrap_with_notice(
    ctx: BashSpawnContext,
    mode: BashSandboxMode,
    private_tmp: &Path,
    writable: &[PathBuf],
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<(BashSpawnContext, Option<String>), ToolError> {
    wrap_for(current_platform(), ctx, mode, private_tmp, writable, which)
}

/// Wraps the spawn context so the command runs write-confined: writes are allowed only under
/// `ctx.cwd`, `writable` and `private_tmp` (exported as `TMPDIR`).
pub fn wrap(
    ctx: BashSpawnContext,
    mode: BashSandboxMode,
    private_tmp: &Path,
    writable: &[PathBuf],
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<BashSpawnContext, ToolError> {
    wrap_with_notice(ctx, mode, private_tmp, writable, which).map(|(ctx, _)| ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx(command: &str) -> BashSpawnContext {
        BashSpawnContext {
            command: command.into(),
            cwd: "/work/proj".into(),
            env: BTreeMap::new(),
        }
    }
    fn found(name: &str) -> Option<PathBuf> {
        Some(PathBuf::from(format!("/usr/bin/{name}")))
    }

    #[test]
    fn sandbox_linux_arguments_are_exact() {
        let text = linux_command(
            Path::new("/usr/bin/bwrap"),
            Path::new("/bin/bash"),
            Path::new("/work/proj"),
            Path::new("/a/tmp/bash-s1"),
            &[PathBuf::from("/a/sessions"), PathBuf::from("/a/cache")],
            "echo 'hi'",
        );
        assert_eq!(
            text,
            "'/usr/bin/bwrap' --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --bind '/a/tmp/bash-s1' '/a/tmp/bash-s1' --setenv TMPDIR '/a/tmp/bash-s1' --bind '/work/proj' '/work/proj' --bind '/a/sessions' '/a/sessions' --bind '/a/cache' '/a/cache' --chdir '/work/proj' --die-with-parent --new-session -- '/bin/bash' -c 'echo '\\''hi'\\'''"
        );
    }

    #[test]
    fn sandbox_macos_profile_is_escaped_and_exact() {
        let profile = macos_profile(
            Path::new("/work/my \"proj\""),
            Path::new("/a/tmp/bash-s1"),
            &[PathBuf::from("/a/sessions")],
        );
        assert_eq!(
            profile,
            "(version 1)(allow default)(deny file-write*)(allow file-write* (subpath \"/work/my \\\"proj\\\"\") (subpath \"/a/sessions\") (subpath \"/a/tmp/bash-s1\") (literal \"/dev/null\") (literal \"/dev/tty\"))"
        );
        let text = macos_command(
            Path::new("/usr/bin/sandbox-exec"),
            Path::new("/bin/bash"),
            "(p)",
            "ls",
        );
        assert_eq!(text, "'/usr/bin/sandbox-exec' -p '(p)' '/bin/bash' -c 'ls'");
    }

    #[test]
    fn sandbox_off_leaves_context_untouched() {
        let (out, notice) = wrap_for(
            Platform::Linux,
            ctx("ls"),
            BashSandboxMode::Off,
            Path::new("/t"),
            &[],
            &|_| None,
        )
        .expect("test");
        assert_eq!(out.command, "ls");
        assert!(notice.is_none());
    }

    #[test]
    fn sandbox_auto_without_tool_runs_unwrapped_with_warning() {
        for platform in [Platform::Linux, Platform::MacOs, Platform::Other] {
            let (out, notice) = wrap_for(
                platform,
                ctx("ls"),
                BashSandboxMode::Auto,
                Path::new("/t"),
                &[],
                &|_| None,
            )
            .expect("test");
            assert_eq!(out.command, "ls");
            assert!(notice.expect("test").starts_with("bash sandbox unavailable: "));
        }
    }

    #[test]
    fn sandbox_required_without_tool_fails_closed() {
        for (platform, tool) in [
            (Platform::Linux, "bwrap"),
            (Platform::MacOs, "sandbox-exec"),
            (Platform::Other, "sandbox-exec"),
        ] {
            let error = wrap_for(
                platform,
                ctx("ls"),
                BashSandboxMode::Required,
                Path::new("/t"),
                &[],
                &|_| None,
            )
            .err().expect("test");
            assert_eq!(
                error.to_string(),
                format!("bashSandbox is \"required\" but {tool} is not available on this system")
            );
        }
    }

    #[test]
    fn sandbox_wrap_sets_tmpdir_and_rewrites_command() {
        let (out, _) = wrap_for(
            Platform::Linux,
            ctx("ls"),
            BashSandboxMode::Required,
            Path::new("/t/p"),
            &[],
            &found,
        )
        .expect("test");
        assert!(out.command.starts_with("'/usr/bin/bwrap' --ro-bind / / "));
        assert_eq!(out.env.get("TMPDIR").map(String::as_str), Some("/t/p"));
        let (out, _) = wrap_for(
            Platform::MacOs,
            ctx("ls"),
            BashSandboxMode::Auto,
            Path::new("/t/p"),
            &[],
            &found,
        )
        .expect("test");
        assert!(
            out.command
                .starts_with("'/usr/bin/sandbox-exec' -p '(version 1)")
        );
    }

    #[test]
    fn sandbox_custom_agent_dir_holds_private_tmp() {
        let home = tempfile::tempdir().expect("test");
        let agent = home.path().join("custom agent");
        let (tmp, writable) = prepare_dirs(&agent, "sess/1").expect("test");
        assert_eq!(tmp, agent.join("tmp").join("bash-sess_1"));
        assert!(tmp.is_dir());
        assert_eq!(writable, vec![agent.join("sessions"), agent.join("cache")]);
    }

    #[test]
    fn sandbox_mode_parse_defaults_to_off() {
        assert_eq!(BashSandboxMode::parse("auto"), BashSandboxMode::Auto);
        assert_eq!(
            BashSandboxMode::parse("required"),
            BashSandboxMode::Required
        );
        assert_eq!(BashSandboxMode::parse("bogus"), BashSandboxMode::Off);
    }
}
