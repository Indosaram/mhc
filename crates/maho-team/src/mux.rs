//! Terminal multiplexer backends: detection, opening a pane (or window) that runs a command, and
//! closing it. tmux argument handling is ported from the MIT oh-my-pi terminal launch provider
//! (`subprocess/terminal-launch/tmux.ts`: `-c` with `#` doubled, trailing-semicolon escaping,
//! `-P -F` id reply, multi-argument direct execution), the split-off-the-lead's-pane without focus
//! (`split-window -d -t $TMUX_PANE`) and `kill-pane` from MIT pi-interactive-subagents, and the
//! cmux `new-split` JSON reply handling from oh-my-pi `terminal-launch/cmux.ts`.
use crate::settings::{MuxKind, PaneSettings, Placement};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRequest {
    pub title: String,
    pub cwd: PathBuf,
    /// Executed directly (multi-argument).
    pub command: Vec<String>,
    pub placement: Placement,
    /// Pane to split off (`None` = the lead's pane).
    pub anchor: Option<String>,
    /// First pane of this process (split to the side; later ones stack below).
    pub first: bool,
}

/// An opened pane or window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneHandle {
    pub id: String,
    pub window: bool,
}

pub trait PaneBackend: Send + Sync {
    fn kind(&self) -> MuxKind;
    /// # Errors
    /// The multiplexer CLI failed or replied with something that is not an id.
    fn open(&self, request: &PaneRequest) -> Result<PaneHandle, String>;
    /// # Errors
    /// The multiplexer CLI failed (for example the pane is already gone).
    fn close(&self, handle: &PaneHandle) -> Result<(), String>;
}

/// The first executable `name` in `path` (a `PATH`-style list).
pub fn which(name: &str, path: Option<&str>) -> Option<PathBuf> {
    let path = path?;
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn set(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key).filter(|value| !value.trim().is_empty())
}

/// The backend for this environment (spec §4.1), or `None` for headless.
pub fn detect(
    settings: &PaneSettings,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<Box<dyn PaneBackend>> {
    if settings.mode == crate::settings::PaneMode::Off {
        return None;
    }
    let path = env("PATH");
    let tmux = || -> Option<Box<dyn PaneBackend>> {
        set(env, "TMUX")?;
        let program = which("tmux", path.as_deref())?;
        Some(Box::new(TmuxBackend {
            program,
            prefix_args: Vec::new(),
            lead_pane: set(env, "TMUX_PANE"),
        }))
    };
    let cmux = || -> Option<Box<dyn PaneBackend>> {
        let workspace = set(env, "CMUX_WORKSPACE_ID");
        let surface = set(env, "CMUX_SURFACE_ID");
        if workspace.is_none() && surface.is_none() {
            return None;
        }
        let program = which("cmux", path.as_deref())?;
        Some(Box::new(CmuxBackend {
            program,
            workspace,
            surface,
        }))
    };
    match settings.preference {
        Some(MuxKind::Tmux) => tmux(),
        Some(MuxKind::Cmux) => cmux(),
        None => tmux().or_else(cmux),
    }
}

fn run(program: &Path, args: &[String]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run {}: {error}", program.display()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let operation = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .cloned()
            .unwrap_or_default();
        Err(match output.status.code() {
            Some(code) => format!("{operation} failed (exit {code})"),
            None => format!("{operation} did not exit successfully"),
        })
    }
}

/// tmux consumes a backslash before a trailing `;` (its command separator); keep existing
/// backslashes and add one.
pub fn escape_tmux_argument(value: &str) -> String {
    let Some(body) = value.strip_suffix(';') else {
        return value.to_owned();
    };
    let backslashes = body.len() - body.trim_end_matches('\\').len();
    let stem = &body[..body.len() - backslashes];
    format!("{stem}{};", "\\".repeat(backslashes + 1))
}

/// tmux backend.
#[derive(Clone, Debug)]
pub struct TmuxBackend {
    pub program: PathBuf,
    /// Placed before every command (e.g. `-L <socket>` in tests).
    pub prefix_args: Vec<String>,
    /// `$TMUX_PANE` of the lead.
    pub lead_pane: Option<String>,
}

impl TmuxBackend {
    /// The command line (after `prefix_args`) that opens `request` as a split (`window = false`)
    /// or a window.
    pub fn open_args(&self, request: &PaneRequest, window: bool) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if window {
            args.extend([
                "new-window".into(),
                "-d".into(),
                "-n".into(),
                short_name(&request.title),
            ]);
        } else {
            args.extend([
                "split-window".into(),
                "-d".into(),
                if request.first { "-h" } else { "-v" }.into(),
            ]);
        }
        // tmux format-expands the start directory; `##` is a literal `#`.
        args.push("-c".into());
        args.push(request.cwd.to_string_lossy().replace('#', "##"));
        if !window && let Some(target) = request.anchor.as_ref().or(self.lead_pane.as_ref()) {
            args.push("-t".into());
            args.push(target.clone());
        }
        args.extend([
            "-P".into(),
            "-F".into(),
            if window { "#{window_id}" } else { "#{pane_id}" }.into(),
            "--".into(),
        ]);
        args.extend(request.command.iter().cloned());
        args.into_iter()
            .map(|arg| escape_tmux_argument(&arg))
            .collect()
    }

    fn command(&self, args: Vec<String>) -> Result<String, String> {
        let mut all = self.prefix_args.clone();
        all.extend(args);
        run(&self.program, &all)
    }

    fn open_as(&self, request: &PaneRequest, window: bool) -> Result<PaneHandle, String> {
        let reply = self.command(self.open_args(request, window))?;
        let id = reply.trim().to_owned();
        let sigil = if window { '@' } else { '%' };
        let valid =
            id.len() > 1 && id.starts_with(sigil) && id[1..].bytes().all(|b| b.is_ascii_digit());
        if !valid {
            return Err(format!(
                "tmux {} returned an invalid id",
                if window { "new-window" } else { "split-window" }
            ));
        }
        if !window {
            let _ = self.command(vec![
                "select-pane".into(),
                "-t".into(),
                id.clone(),
                "-T".into(),
                request.title.clone(),
            ]);
        }
        Ok(PaneHandle { id, window })
    }
}

fn short_name(title: &str) -> String {
    title.chars().take(30).collect()
}

impl PaneBackend for TmuxBackend {
    fn kind(&self) -> MuxKind {
        MuxKind::Tmux
    }

    fn open(&self, request: &PaneRequest) -> Result<PaneHandle, String> {
        match request.placement {
            Placement::Window => self.open_as(request, true),
            // A split can fail for lack of space; a window always fits.
            Placement::Split => self.open_as(request, false).or_else(|split_error| {
                self.open_as(request, true)
                    .map_err(|window_error| format!("{split_error}; {window_error}"))
            }),
        }
    }

    fn close(&self, handle: &PaneHandle) -> Result<(), String> {
        let verb = if handle.window {
            "kill-window"
        } else {
            "kill-pane"
        };
        self.command(vec![verb.into(), "-t".into(), handle.id.clone()])
            .map(|_| ())
    }
}

/// Single-quotes `value` for a POSIX shell.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// cmux backend.
#[derive(Clone, Debug)]
pub struct CmuxBackend {
    pub program: PathBuf,
    pub workspace: Option<String>,
    pub surface: Option<String>,
}

fn cmux_payload(value: &Value) -> &Value {
    if value.get("ok") == Some(&Value::Bool(false)) {
        return value;
    }
    match value.get("data") {
        Some(data) if data.is_object() => data,
        _ => value,
    }
}

fn cmux_id(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match value.get(*key)? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

impl CmuxBackend {
    pub fn open_args(&self, request: &PaneRequest) -> Vec<String> {
        let command = request
            .command
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let shell = format!(
            "cd {} && {command}",
            shell_quote(&request.cwd.to_string_lossy())
        );
        let mut args: Vec<String> = vec![
            "--json".into(),
            "new-split".into(),
            if request.first { "right" } else { "down" }.into(),
        ];
        match (&request.anchor, &self.surface, &self.workspace) {
            (Some(anchor), _, _) => args.extend(["--surface".into(), anchor.clone()]),
            (None, Some(surface), _) => args.extend(["--surface".into(), surface.clone()]),
            (None, None, Some(workspace)) => args.extend(["--workspace".into(), workspace.clone()]),
            (None, None, None) => {}
        }
        args.extend(["--command".into(), shell, "--focus".into(), "false".into()]);
        args
    }
}

impl PaneBackend for CmuxBackend {
    fn kind(&self) -> MuxKind {
        MuxKind::Cmux
    }

    fn open(&self, request: &PaneRequest) -> Result<PaneHandle, String> {
        let reply = run(&self.program, &self.open_args(request))?;
        let value: Value = serde_json::from_str(reply.trim())
            .map_err(|_| "cmux new-split returned invalid JSON".to_owned())?;
        let payload = cmux_payload(&value);
        if payload.get("ok") == Some(&Value::Bool(false)) {
            return Err("cmux new-split returned an unsuccessful response".into());
        }
        let id = cmux_id(
            payload,
            &["surface_id", "surface_ref", "pane_id", "pane_ref"],
        )
        .ok_or_else(|| "cmux new-split returned no id".to_owned())?;
        Ok(PaneHandle { id, window: false })
    }

    fn close(&self, handle: &PaneHandle) -> Result<(), String> {
        run(
            &self.program,
            &[
                "close-surface".into(),
                "--surface".into(),
                handle.id.clone(),
            ],
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(first: bool, anchor: Option<&str>) -> PaneRequest {
        PaneRequest {
            title: "team a · task A-x".into(),
            cwd: PathBuf::from("/w/#1"),
            command: vec!["/bin/sh".into(), "-c".into(), "echo hi;".into()],
            placement: Placement::Split,
            anchor: anchor.map(str::to_owned),
            first,
        }
    }

    #[test]
    fn escapes_trailing_semicolons() {
        assert_eq!(escape_tmux_argument("a;"), "a\\;");
        assert_eq!(escape_tmux_argument("a\\;"), "a\\\\;");
        assert_eq!(escape_tmux_argument("a;b"), "a;b");
    }

    #[test]
    fn tmux_arguments() {
        let backend = TmuxBackend {
            program: "tmux".into(),
            prefix_args: Vec::new(),
            lead_pane: Some("%0".into()),
        };
        assert_eq!(
            backend.open_args(&request(true, None), false),
            [
                "split-window",
                "-d",
                "-h",
                "-c",
                "/w/##1",
                "-t",
                "%0",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "/bin/sh",
                "-c",
                "echo hi\\;"
            ]
        );
        assert_eq!(
            backend.open_args(&request(false, Some("%3")), false)[..7],
            ["split-window", "-d", "-v", "-c", "/w/##1", "-t", "%3"]
        );
        assert_eq!(
            backend.open_args(&request(true, None), true)[..9],
            [
                "new-window",
                "-d",
                "-n",
                "team a · task A-x",
                "-c",
                "/w/##1",
                "-P",
                "-F",
                "#{window_id}"
            ]
        );
    }

    #[test]
    fn cmux_arguments_and_reply() {
        let backend = CmuxBackend {
            program: "cmux".into(),
            workspace: Some("w1".into()),
            surface: Some("s1".into()),
        };
        let args = backend.open_args(&request(true, None));
        assert_eq!(
            args[..5],
            ["--json", "new-split", "right", "--surface", "s1"]
        );
        assert_eq!(args[6], "cd '/w/#1' && '/bin/sh' '-c' 'echo hi;'");
        let value: Value =
            serde_json::from_str(r#"{"ok":true,"data":{"pane_ref":"pane:4"}}"#).expect("json");
        assert_eq!(
            cmux_id(cmux_payload(&value), &["surface_id", "pane_ref"]).as_deref(),
            Some("pane:4")
        );
    }

    #[cfg(unix)]
    #[test]
    fn detection() {
        let dir = tempfile::tempdir().expect("tmp");
        for name in ["tmux", "cmux"] {
            let path = dir.path().join(name);
            std::fs::write(&path, "#!/bin/sh\n").expect("write");
            let mut perms = std::fs::metadata(&path).expect("meta").permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&path, perms).expect("chmod");
        }
        let path = dir.path().to_string_lossy().into_owned();
        let settings = PaneSettings::default();
        let env_with = |pairs: Vec<(&'static str, String)>| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.clone())
            }
        };
        let tmux_env = env_with(vec![
            ("PATH", path.clone()),
            ("TMUX", "/tmp/s,1,0".into()),
            ("CMUX_SURFACE_ID", "s".into()),
        ]);
        assert_eq!(
            detect(&settings, &tmux_env).map(|b| b.kind()),
            Some(MuxKind::Tmux)
        );
        let prefer_cmux = PaneSettings {
            preference: Some(MuxKind::Cmux),
            ..PaneSettings::default()
        };
        assert_eq!(
            detect(&prefer_cmux, &tmux_env).map(|b| b.kind()),
            Some(MuxKind::Cmux)
        );
        let off = PaneSettings {
            mode: crate::settings::PaneMode::Off,
            ..PaneSettings::default()
        };
        assert!(detect(&off, &tmux_env).is_none());
        let no_session = env_with(vec![("PATH", path.clone())]);
        assert!(detect(&settings, &no_session).is_none());
        let no_binary = env_with(vec![("PATH", "/nonexistent".into()), ("TMUX", "x".into())]);
        assert!(detect(&settings, &no_binary).is_none());
    }
}
