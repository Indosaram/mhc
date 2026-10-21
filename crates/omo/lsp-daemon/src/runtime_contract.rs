use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub const OMO_LSP_DAEMON_DIR: &str = "MAHO_LSP_DAEMON_DIR";
pub const OMO_LSP_DAEMON_CLI: &str = "OMO_LSP_DAEMON_CLI";
pub const OMO_LSP_DAEMON_VERSION: &str = "OMO_LSP_DAEMON_VERSION";

pub type Env = BTreeMap<String, String>;

pub fn process_env() -> Env {
    std::env::vars().collect()
}

fn is_valid_daemon_version(version: &str) -> bool {
    let mut characters = version.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    let rest: Vec<char> = characters.collect();
    rest.len() <= 127
        && rest
            .iter()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '+' | '-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDaemonVersionError {
    pub version: String,
}

impl InvalidDaemonVersionError {
    pub const CODE: &'static str = "invalid_daemon_version";

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl fmt::Display for InvalidDaemonVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LSP daemon version must match [A-Za-z0-9][A-Za-z0-9._+-]{0,127}")
    }
}

impl std::error::Error for InvalidDaemonVersionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidRuntimeOverrideReason {
    PairedValuesRequired,
    CliMustBeAbsolute,
    CliNotFound,
    CliNotFile,
    PackagedCliMustBeAbsolute,
}

impl InvalidRuntimeOverrideReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PairedValuesRequired => "paired_values_required",
            Self::CliMustBeAbsolute => "cli_must_be_absolute",
            Self::CliNotFound => "cli_not_found",
            Self::CliNotFile => "cli_not_file",
            Self::PackagedCliMustBeAbsolute => "packaged_cli_must_be_absolute",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRuntimeOverrideError {
    pub reason: InvalidRuntimeOverrideReason,
    pub message: String,
}

impl InvalidRuntimeOverrideError {
    pub const CODE: &'static str = "invalid_runtime_override";

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl fmt::Display for InvalidRuntimeOverrideError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InvalidRuntimeOverrideError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonRuntimeError {
    Override(InvalidRuntimeOverrideError),
    Version(InvalidDaemonVersionError),
}

impl DaemonRuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Override(error) => error.code(),
            Self::Version(error) => error.code(),
        }
    }
}

impl fmt::Display for DaemonRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Override(error) => error.fmt(formatter),
            Self::Version(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DaemonRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Override(error) => Some(error),
            Self::Version(error) => Some(error),
        }
    }
}

impl From<InvalidRuntimeOverrideError> for DaemonRuntimeError {
    fn from(error: InvalidRuntimeOverrideError) -> Self {
        Self::Override(error)
    }
}

impl From<InvalidDaemonVersionError> for DaemonRuntimeError {
    fn from(error: InvalidDaemonVersionError) -> Self {
        Self::Version(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRuntime {
    pub cli_path: PathBuf,
    pub version: String,
}

pub type DaemonRuntimeDefaults = DaemonRuntime;

pub fn validate_daemon_version(version: &str) -> Result<String, InvalidDaemonVersionError> {
    if is_valid_daemon_version(version) {
        Ok(version.to_owned())
    } else {
        Err(InvalidDaemonVersionError {
            version: version.to_owned(),
        })
    }
}

fn stat_is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

pub fn resolve_daemon_runtime(
    env: &Env,
    defaults: &DaemonRuntimeDefaults,
) -> Result<DaemonRuntime, DaemonRuntimeError> {
    let cli_override = env.get(OMO_LSP_DAEMON_CLI);
    let version_override = env.get(OMO_LSP_DAEMON_VERSION);

    if cli_override.is_some() != version_override.is_some() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::PairedValuesRequired,
            message: format!(
                "{OMO_LSP_DAEMON_CLI} and {OMO_LSP_DAEMON_VERSION} must be set together"
            ),
        }.into());
    }

    let (Some(cli_override), Some(version_override)) = (cli_override, version_override) else {
        if !defaults.cli_path.is_absolute() {
            return Err(InvalidRuntimeOverrideError {
                reason: InvalidRuntimeOverrideReason::PackagedCliMustBeAbsolute,
                message: String::from("Packaged LSP daemon CLI path must be absolute"),
            }.into());
        }
        return Ok(DaemonRuntime {
            cli_path: defaults.cli_path.clone(),
            version: validate_daemon_version(&defaults.version)?,
        });
    };

    if !Path::new(cli_override).is_absolute() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::CliMustBeAbsolute,
            message: format!(
                "{OMO_LSP_DAEMON_CLI} must be an absolute path to an existing regular file"
            ),
        }.into());
    }

    if !stat_is_file(Path::new(cli_override)) {
        let exists = std::fs::metadata(cli_override).is_ok();
        return Err(InvalidRuntimeOverrideError {
            reason: if exists {
                InvalidRuntimeOverrideReason::CliNotFile
            } else {
                InvalidRuntimeOverrideReason::CliNotFound
            },
            message: format!("{OMO_LSP_DAEMON_CLI} must name an existing regular file"),
        }.into());
    }

    Ok(DaemonRuntime {
        cli_path: PathBuf::from(cli_override),
        version: validate_daemon_version(version_override)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_packaged_and_override_versions_preserve_typed_version_error() {
        let root = tempfile::tempdir().expect("temporary runtime directory");
        let cli = root.path().join("cli");
        std::fs::write(&cli, "").expect("runtime CLI fixture");
        let defaults = DaemonRuntime { cli_path: cli.clone(), version: "../bad".to_string() };
        let override_env = Env::from([
            (OMO_LSP_DAEMON_CLI.to_string(), cli.to_string_lossy().into_owned()),
            (OMO_LSP_DAEMON_VERSION.to_string(), "../bad".to_string()),
        ]);
        for env in [Env::new(), override_env] {
            let error = resolve_daemon_runtime(&env, &defaults).expect_err("invalid version");
            assert_eq!(error.code(), "invalid_daemon_version");
            assert_eq!(error, DaemonRuntimeError::Version(InvalidDaemonVersionError {
                version: "../bad".to_string(),
            }));
        }
    }
}
