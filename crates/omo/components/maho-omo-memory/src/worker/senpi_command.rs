use std::{collections::BTreeMap, path::Path, sync::Arc};
use senpi_task::runners::rpc::spawn::{RpcSpawnRuntime, resolve_senpi_launcher};
use super::model_preflight::Launcher;

pub struct SenpiLaunchRuntime<'a> {
    pub is_bun_binary: bool,
    pub exec_path: &'a str,
    pub platform: &'a str,
    pub argv: &'a [String],
    pub resolve_installed_cli: &'a dyn Fn() -> Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SenpiLaunchError;
impl std::fmt::Display for SenpiLaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Unable to resolve a runnable Senpi launcher") }
}
impl std::error::Error for SenpiLaunchError {}

pub fn resolve_senpi_launch(env: &BTreeMap<String, String>, runtime: &SenpiLaunchRuntime<'_>) -> Result<Launcher, SenpiLaunchError> {
    let task_runtime = RpcSpawnRuntime { is_bun_binary: runtime.is_bun_binary, exec_path: runtime.exec_path.into(), platform: runtime.platform.into(), parent_env: env.clone(), resolve_rpc_entry: Arc::new(String::new), resolve_senpi_executable: None };
    if let Some(launcher) = resolve_senpi_launcher(&task_runtime) { return Ok(Launcher { command: launcher.command, prefix_args: launcher.prefix_args }); }
    if let Some(installed) = (runtime.resolve_installed_cli)() { return Ok(Launcher { command: runtime.exec_path.into(), prefix_args: vec![installed] }); }
    if let Some(entry) = runtime.argv.get(1).filter(|entry| Path::new(entry).is_absolute() && Path::new(entry).exists()) { return Ok(Launcher { command: runtime.exec_path.into(), prefix_args: vec![entry.clone()] }); }
    Err(SenpiLaunchError)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn installed_cli_retains_interpreter() { let root = tempfile::tempdir().unwrap(); let cli = root.path().join("cli.js"); std::fs::write(&cli, "").unwrap(); let lookup = || Some(cli.to_string_lossy().into_owned()); let launcher = resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, exec_path: "node", platform: "linux", argv: &[], resolve_installed_cli: &lookup }).unwrap(); assert_eq!(launcher.command, "node"); assert_eq!(launcher.prefix_args, [cli.to_string_lossy()]); }
    #[test] fn current_absolute_entry_is_fallback() { let root = tempfile::tempdir().unwrap(); let entry = root.path().join("cli.js"); std::fs::write(&entry, "").unwrap(); let argv = vec!["node".into(), entry.to_string_lossy().into_owned()]; let launcher = resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, exec_path: "node", platform: "linux", argv: &argv, resolve_installed_cli: &|| None }).unwrap(); assert_eq!(launcher.prefix_args, [argv[1].clone()]); }
    #[test] fn missing_executable_never_launches_bare_interpreter() { assert!(resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, exec_path: "node", platform: "linux", argv: &[], resolve_installed_cli: &|| None }).is_err()); }
    #[test] fn windows_npm_shim_resolves_adjacent_cli() { let root = tempfile::tempdir().unwrap(); let shim = root.path().join("senpi.cmd"); let cli = root.path().join("node_modules/@code-yeongyu/senpi/dist/cli.js"); std::fs::create_dir_all(cli.parent().unwrap()).unwrap(); std::fs::write(&shim, "@echo off\r\n").unwrap(); std::fs::write(&cli, "").unwrap(); let env = BTreeMap::from([("PATH".into(), "".into()), ("SENPI_BIN".into(), shim.to_string_lossy().into_owned())]); let launcher = resolve_senpi_launch(&env, &SenpiLaunchRuntime { is_bun_binary: false, exec_path: "C:\\node.exe", platform: "win32", argv: &[], resolve_installed_cli: &|| None }).unwrap(); assert_eq!(launcher.command, "C:\\node.exe"); assert_eq!(launcher.prefix_args, [cli.canonicalize().unwrap().to_string_lossy()]); }
    #[test] fn restricted_path_launches_real_script() { let root = tempfile::tempdir().unwrap(); let entry = root.path().join("entry.sh"); std::fs::write(&entry, "printf 'LAUNCH_OK\\n'\n").unwrap(); let lookup = || Some(entry.to_string_lossy().into_owned()); let launcher = resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, exec_path: "/bin/sh", platform: "linux", argv: &[], resolve_installed_cli: &lookup }).unwrap(); let output = std::process::Command::new(launcher.command).args(launcher.prefix_args).env_clear().output().unwrap(); assert!(output.status.success()); assert_eq!(output.stdout, b"LAUNCH_OK\n"); }
}
