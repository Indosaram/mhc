use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::io::AsyncReadExt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsInstallerChoice {
    #[default]
    Auto,
    Bun,
    Npm,
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct EnvironmentError {
    pub code: &'static str,
    pub message: String,
}
impl EnvironmentError {
    pub fn failed(message: impl ToString) -> Self {
        Self {
            code: "environment_install_failed",
            message: message.to_string(),
        }
    }
    pub fn cancelled() -> Self {
        Self {
            code: "environment_install_cancelled",
            message: "the install was cancelled".into(),
        }
    }
}
impl From<std::io::Error> for EnvironmentError {
    fn from(error: std::io::Error) -> Self {
        Self::failed(error)
    }
}
pub type InstallOutput = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub fn resolve_js_installer(
    choice: JsInstallerChoice,
    env: &HashMap<String, String>,
) -> Result<(&'static str, PathBuf), EnvironmentError> {
    let order: &[&str] = match choice {
        JsInstallerChoice::Auto => &["bun", "npm"],
        JsInstallerChoice::Bun => &["bun"],
        JsInstallerChoice::Npm => &["npm"],
    };
    let path = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value);
    for &name in order {
        for directory in std::env::split_paths(&path.cloned().unwrap_or_default()) {
            if directory.as_os_str().is_empty() {
                continue;
            }
            for extension in if cfg!(windows) {
                &[".exe", ".cmd", ""][..]
            } else {
                &[""][..]
            } {
                let candidate = directory.join(format!("{name}{extension}"));
                if candidate.exists() {
                    return Ok((name, candidate));
                }
            }
        }
    }
    Err(EnvironmentError {
        code: "environment_installer_unavailable",
        message: format!("{} is not on PATH", order.join(" or ")),
    })
}

pub fn parse_js_packages(text: &str) -> Result<Vec<String>, EnvironmentError> {
    if text
        .chars()
        .any(|ch| matches!(u32::from(ch),0..=8|14..=31|127))
    {
        return Err(EnvironmentError::failed(
            "package names cannot contain control characters",
        ));
    }
    let packages = text
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(flag) = packages.iter().find(|token| token.starts_with('-')) {
        return Err(EnvironmentError::failed(format!(
            "installer flags are chosen by the host; name packages only (got {flag})"
        )));
    }
    if packages.is_empty() {
        return Err(EnvironmentError::failed("name at least one package to add"));
    }
    Ok(packages)
}
pub fn absolute_spec(spec: &str, cwd: &Path) -> String {
    let (prefix, path) = spec
        .strip_prefix("file:")
        .map_or(("", spec), |path| ("file:", path));
    if path.starts_with("./") || path.starts_with("../") || matches!(path, "." | "..") {
        format!("{prefix}{}", cwd.join(path).to_string_lossy())
    } else {
        spec.into()
    }
}
pub fn js_install_argv(
    installer: &str,
    root: &Path,
    packages: &[String],
    remove: Option<&str>,
) -> Vec<String> {
    let verb = match (installer, remove.is_some()) {
        ("bun", false) => "add",
        ("bun", true) => "remove",
        (_, false) => "install",
        (_, true) => "uninstall",
    };
    let mut argv = vec![verb.into(), "--ignore-scripts".into()];
    if installer == "bun" {
        argv.push("--cwd".into());
    } else {
        argv.extend(["--no-audit".into(), "--no-fund".into(), "--prefix".into()]);
    }
    argv.push(root.to_string_lossy().into_owned());
    match remove {
        Some(name) => argv.push(name.into()),
        None => argv.extend_from_slice(packages),
    }
    argv
}

pub async fn requested_package_name(spec: &str) -> Option<String> {
    let path = Path::new(spec.strip_prefix("file:").unwrap_or(spec));
    if path.is_absolute() {
        let text = if path.is_dir() {
            tokio::fs::read(path.join("package.json")).await.ok()?
        } else {
            let list = tokio::process::Command::new("tar")
                .args(["-tf"])
                .arg(path)
                .output()
                .await
                .ok()?;
            if !list.status.success() {
                return None;
            }
            let entries = String::from_utf8(list.stdout).ok()?;
            let mut tops = entries
                .lines()
                .map(|line| {
                    line.trim_start_matches("./")
                        .split('/')
                        .next()
                        .unwrap_or("")
                })
                .filter(|top| !top.is_empty())
                .collect::<Vec<_>>();
            tops.sort_unstable();
            tops.dedup();
            if tops.len() != 1 || matches!(tops[0], "." | "..") {
                return None;
            }
            let entry = entries.lines().find(|line| {
                line.trim_start_matches("./") == format!("{}/package.json", tops[0])
            })?;
            let output = tokio::process::Command::new("tar")
                .arg("-xOf")
                .arg(path)
                .arg(entry)
                .output()
                .await
                .ok()?;
            if !output.status.success() {
                return None;
            }
            output.stdout
        };
        return serde_json::from_slice::<serde_json::Value>(&text)
            .ok()?
            .get("name")?
            .as_str()
            .map(str::to_owned);
    }
    // PERF-DIVERGENCE(senpi js-installer): upstream builds the package-name regex on each call; this port compiles it once in a LazyLock to avoid per-spec recompilation. Matching is identical.
    static PACKAGE_NAME: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^((?:@[a-z0-9][\w.-]*/)?[a-z0-9][\w.-]*)(?:@[^/]*)?$").ok()
    });
    PACKAGE_NAME
        .as_ref()?
        .captures(spec)?
        .get(1)
        .map(|name| name.as_str().into())
}

pub struct JsInstall<'a> {
    pub installer: &'a str,
    pub command: &'a Path,
    pub root: &'a Path,
    pub packages: &'a [String],
    pub remove: Option<&'a str>,
    pub recorded_specs: &'a [String],
    pub cwd: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub signal: &'a maho_ext_api::AbortSignal,
    pub on_output: Option<&'a InstallOutput>,
}
pub fn without_host_paths(text: &str, input: &JsInstall<'_>) -> String {
    let mut specs = input
        .packages
        .iter()
        .map(|spec| spec.strip_prefix("file:").unwrap_or(spec))
        .filter(|path| Path::new(path).is_absolute())
        .chain(input.recorded_specs.iter().map(String::as_str))
        .collect::<Vec<_>>();
    specs.sort_by_key(|path| std::cmp::Reverse(path.len()));
    let mut text = text.to_owned();
    for path in specs {
        text = text.replace(
            path,
            &format!(
                "<path>/{}",
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );
    }
    for (path, label) in [
        (Some(input.root.to_path_buf()), "<root>"),
        (Some(input.cwd.to_path_buf()), "<cwd>"),
        (std::fs::canonicalize(std::env::temp_dir()).ok(), "<tmp>"),
        (Some(std::env::temp_dir()), "<tmp>"),
        (
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            "~",
        ),
    ] {
        if let Some(path) = path
            && path.parent().is_some()
        {
            text = text.replace(path.to_string_lossy().as_ref(), label);
        }
    }
    text
}

pub async fn run_js_install(input: JsInstall<'_>) -> Result<(), EnvironmentError> {
    if input.signal.is_aborted() {
        return Err(EnvironmentError::cancelled());
    }
    let mut command = tokio::process::Command::new(input.command);
    command
        .args(js_install_argv(
            input.installer,
            input.root,
            input.packages,
            input.remove,
        ))
        .current_dir(input.cwd)
        .env_clear()
        .envs(input.env)
        .env("npm_config_ignore_scripts", "true")
        .env("npm_config_global", "false")
        .env("npm_config_location", "project")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| EnvironmentError {
        code: "environment_installer_unavailable",
        message: without_host_paths(&error.to_string(), &input),
    })?;
    let group = InstallProcessGroup(child.id());
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| EnvironmentError::failed("installer stdout unavailable"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| EnvironmentError::failed("installer stderr unavailable"))?;
    let mut out = [0; 4096];
    let mut err = [0; 4096];
    let mut out_open = true;
    let mut err_open = true;
    let mut tail = String::new();
    let status = loop {
        tokio::select! {
            biased;
            ()=input.signal.cancelled()=>{drop(group);child.kill().await?;return Err(EnvironmentError::cancelled());}
            result=stdout.read(&mut out),if out_open=>{let n=result?;out_open=n!=0;if n>0 {record("stdout",&out[..n],&input,&mut tail);}}
            result=stderr.read(&mut err),if err_open=>{let n=result?;err_open=n!=0;if n>0 {record("stderr",&err[..n],&input,&mut tail);}}
            result=child.wait(),if !out_open && !err_open=>break result?,
        }
    };
    drop(group);
    if input.signal.is_aborted() {
        return Err(EnvironmentError::cancelled());
    }
    if status.success() {
        Ok(())
    } else {
        let text = without_host_paths(tail.trim(), &input);
        Err(EnvironmentError::failed(if text.is_empty() {
            format!("{} exited with {status}", input.installer)
        } else {
            text
        }))
    }
}
fn record(stream: &str, data: &[u8], input: &JsInstall<'_>, tail: &mut String) {
    let data = String::from_utf8_lossy(data);
    tail.push_str(&data);
    if tail.len() > 4096 {
        let mut boundary = tail.len() - 4096;
        while !tail.is_char_boundary(boundary) {
            boundary += 1;
        }
        tail.drain(..boundary);
    }
    if let Some(output) = input.on_output {
        output(stream, &without_host_paths(&data, input));
    }
}
struct InstallProcessGroup(Option<u32>);
impl Drop for InstallProcessGroup {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        let _pid = self.0;
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
            && let Err(error) =
                rustix::process::kill_process_group(pid, rustix::process::Signal::KILL)
            && error != rustix::io::Errno::SRCH
        {
            maho_ext_api::diagnostic(&format!("installer process-group cleanup failed: {error}"));
        }
    }
}
