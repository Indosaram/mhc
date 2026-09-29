use std::collections::BTreeMap;
use std::path::PathBuf;

use super::paths::codegraph_data_root;

pub const CODEGRAPH_INSTALL_DIR_ENV: &str = "CODEGRAPH_INSTALL_DIR";
pub const CODEGRAPH_NO_DAEMON_ENV: &str = "CODEGRAPH_NO_DAEMON";
pub const CODEGRAPH_NO_DOWNLOAD_ENV: &str = "CODEGRAPH_NO_DOWNLOAD";
pub const CODEGRAPH_TELEMETRY_ENV: &str = "CODEGRAPH_TELEMETRY";
pub const DO_NOT_TRACK_ENV: &str = "DO_NOT_TRACK";

const SAFE_AMBIENT_ENV_KEYS: &[&str] = &[
    "APPDATA",
    "CI",
    "CODEX_HOME",
    "ComSpec",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOCALAPPDATA",
    "PATH",
    "PATHEXT",
    "Path",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USERPROFILE",
    "WINDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

const SAFE_CODEGRAPH_RUNTIME_ENV_KEYS: &[&str] = &[
    "CODEGRAPH_ALLOW_UNSAFE_NODE",
    "CODEGRAPH_BIN",
    "CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS",
    "CODEGRAPH_FAKE_LOG",
    "CODEGRAPH_NO_DAEMON",
    "CODEGRAPH_NODE_BIN",
    "OMO_CODEGRAPH_BIN",
    "OMO_CODEGRAPH_PROJECT_CWD",
    "OMO_CODEGRAPH_SESSION_START_CWD",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCodegraphEnvOptions {
    pub home_dir: Option<PathBuf>,
    pub daemon: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegraphEnv {
    pub install_dir: PathBuf,
    pub daemon: Option<bool>,
}

impl CodegraphEnv {
    pub fn to_map(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        map.insert(
            CODEGRAPH_INSTALL_DIR_ENV.to_string(),
            self.install_dir.to_string_lossy().into_owned(),
        );
        if self.daemon == Some(false) {
            map.insert(CODEGRAPH_NO_DAEMON_ENV.to_string(), "1".to_string());
        }
        map.insert(CODEGRAPH_NO_DOWNLOAD_ENV.to_string(), "1".to_string());
        map.insert(CODEGRAPH_TELEMETRY_ENV.to_string(), "0".to_string());
        map.insert(DO_NOT_TRACK_ENV.to_string(), "1".to_string());
        map
    }
}

pub fn build_codegraph_env(options: &BuildCodegraphEnvOptions) -> CodegraphEnv {
    let home_dir = options
        .home_dir
        .clone()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from(""));
    let install_dir = codegraph_data_root(&home_dir);
    CodegraphEnv {
        install_dir,
        daemon: options.daemon,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCodegraphChildEnvOptions {
    pub ambient_env: Option<BTreeMap<String, String>>,
    pub codegraph_env: Option<BTreeMap<String, String>>,
    pub runtime_env: Option<BTreeMap<String, String>>,
}

pub fn build_codegraph_child_env(
    options: &BuildCodegraphChildEnvOptions,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(ambient) = &options.ambient_env {
        for key in SAFE_AMBIENT_ENV_KEYS {
            if let Some(val) = ambient.get(*key) {
                env.insert(key.to_string(), val.clone());
            }
        }
    }
    if let Some(runtime) = &options.runtime_env {
        for key in SAFE_CODEGRAPH_RUNTIME_ENV_KEYS {
            if let Some(val) = runtime.get(*key) {
                env.insert(key.to_string(), val.clone());
            }
        }
    }
    if let Some(codegraph) = &options.codegraph_env {
        for (k, v) in codegraph {
            env.insert(k.clone(), v.clone());
        }
    }
    env
}
