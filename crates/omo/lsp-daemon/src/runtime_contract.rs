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
) -> Result<DaemonRuntime, InvalidRuntimeOverrideError> {
    let cli_override = env.get(OMO_LSP_DAEMON_CLI);
    let version_override = env.get(OMO_LSP_DAEMON_VERSION);

    if cli_override.is_some() != version_override.is_some() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::PairedValuesRequired,
            message: format!(
                "{OMO_LSP_DAEMON_CLI} and {OMO_LSP_DAEMON_VERSION} must be set together"
            ),
        });
    }

    let (Some(cli_override), Some(version_override)) = (cli_override, version_override) else {
        if !defaults.cli_path.is_absolute() {
            return Err(InvalidRuntimeOverrideError {
                reason: InvalidRuntimeOverrideReason::PackagedCliMustBeAbsolute,
                message: String::from("Packaged LSP daemon CLI path must be absolute"),
            });
        }
        return Ok(DaemonRuntime {
            cli_path: defaults.cli_path.clone(),
            version: validate_daemon_version(&defaults.version).map_err(|error| {
                InvalidRuntimeOverrideError {
                    reason: InvalidRuntimeOverrideReason::PackagedCliMustBeAbsolute,
                    message: error.to_string(),
                }
            })?,
        });
    };

    if !Path::new(cli_override).is_absolute() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::CliMustBeAbsolute,
            message: format!(
                "{OMO_LSP_DAEMON_CLI} must be an absolute path to an existing regular file"
            ),
        });
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
        });
    }

    Ok(DaemonRuntime {
        cli_path: PathBuf::from(cli_override),
        version: validate_daemon_version(version_override).map_err(|error| {
            InvalidRuntimeOverrideError {
                reason: InvalidRuntimeOverrideReason::CliNotFile,
                message: error.to_string(),
            }
        })?,
    })
}
