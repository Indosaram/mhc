use std::path::Path;

pub use lsp_daemon::runtime_contract::{DaemonRuntime as SenpiDaemonRuntime, Env, InvalidRuntimeOverrideError};
use lsp_daemon::runtime_contract::{DaemonRuntimeError, InvalidRuntimeOverrideReason, validate_daemon_version};

pub fn resolve_senpi_daemon_runtime(env: &Env, packaged_runtime: &SenpiDaemonRuntime) -> Result<SenpiDaemonRuntime, DaemonRuntimeError> {
    let cli = env.get("OMO_LSP_DAEMON_CLI");
    let version = env.get("OMO_LSP_DAEMON_VERSION");
    if cli.is_some() != version.is_some() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::PairedValuesRequired,
            message: "OMO_LSP_DAEMON_CLI and OMO_LSP_DAEMON_VERSION must be set together".into(),
        }.into());
    }
    let (Some(cli), Some(version)) = (cli, version) else {
        if !packaged_runtime.cli_path.is_absolute() {
            return Err(InvalidRuntimeOverrideError {
                reason: InvalidRuntimeOverrideReason::PackagedCliMustBeAbsolute,
                message: "Packaged Senpi LSP daemon CLI path must be absolute".into(),
            }.into());
        }
        return Ok(SenpiDaemonRuntime {
            cli_path: packaged_runtime.cli_path.clone(),
            version: validate_daemon_version(&packaged_runtime.version)?,
        });
    };
    if !Path::new(cli).is_absolute() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::CliMustBeAbsolute,
            message: "OMO_LSP_DAEMON_CLI must be an absolute path to an existing regular file".into(),
        }.into());
    }
    let metadata = match std::fs::metadata(cli) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::CliNotFound,
            message: "OMO_LSP_DAEMON_CLI points to a missing file".into(),
        }.into()),
        Err(error) => return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::CliNotFound,
            message: error.to_string(),
        }.into()),
    };
    if !metadata.is_file() {
        return Err(InvalidRuntimeOverrideError {
            reason: InvalidRuntimeOverrideReason::CliNotFile,
            message: "OMO_LSP_DAEMON_CLI must point to a regular file".into(),
        }.into());
    }
    Ok(SenpiDaemonRuntime { cli_path: cli.into(), version: validate_daemon_version(version)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_override_contract_and_exact_errors() {
        let root = tempfile::tempdir().expect("runtime fixture");
        let cli = root.path().join("daemon");
        std::fs::write(&cli, "").expect("override executable fixture");
        let packaged = SenpiDaemonRuntime { cli_path: cli.clone(), version: "0.1.0".into() };
        assert_eq!(resolve_senpi_daemon_runtime(&Env::new(), &packaged).expect("packaged runtime"), packaged);
        let env = |path: &Path, version: &str| Env::from([
            ("OMO_LSP_DAEMON_CLI".into(), path.to_string_lossy().into_owned()),
            ("OMO_LSP_DAEMON_VERSION".into(), version.into()),
        ]);
        assert_eq!(resolve_senpi_daemon_runtime(&env(&cli, "9.8.7"), &packaged).expect("override runtime").version, "9.8.7");
        for (values, message) in [
            (Env::from([("OMO_LSP_DAEMON_CLI".into(), cli.to_string_lossy().into_owned())]), "OMO_LSP_DAEMON_CLI and OMO_LSP_DAEMON_VERSION must be set together"),
            (env(Path::new("relative"), "1"), "OMO_LSP_DAEMON_CLI must be an absolute path to an existing regular file"),
            (env(&root.path().join("missing"), "1"), "OMO_LSP_DAEMON_CLI points to a missing file"),
            (env(root.path(), "1"), "OMO_LSP_DAEMON_CLI must point to a regular file"),
            (env(&cli, "invalid version"), "LSP daemon version must match [A-Za-z0-9][A-Za-z0-9._+-]{0,127}"),
        ] {
            assert_eq!(resolve_senpi_daemon_runtime(&values, &packaged).expect_err("invalid override").to_string(), message);
        }
        let relative = SenpiDaemonRuntime { cli_path: "relative".into(), version: "1".into() };
        assert_eq!(resolve_senpi_daemon_runtime(&Env::new(), &relative).expect_err("relative packaged CLI").to_string(), "Packaged Senpi LSP daemon CLI path must be absolute");
    }
}
