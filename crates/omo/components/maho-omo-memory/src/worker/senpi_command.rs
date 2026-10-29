use std::{collections::BTreeMap, path::{Path,PathBuf}, sync::Arc};
use senpi_task::runners::rpc::spawn::{RpcSpawnRuntime, resolve_senpi_launcher};
use super::model_preflight::Launcher;

pub struct SenpiLaunchRuntime<'a> {
    pub is_bun_binary: bool,
    pub is_compiled_engine: bool,
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

const PACKAGE_DIR_NAMES: [&str;3] = ["OMO_PACKAGE_DIR","SENPI_PACKAGE_DIR","PI_PACKAGE_DIR"];
fn canonical(path:&Path)->PathBuf { std::fs::canonicalize(path).unwrap_or_else(|_|path.to_path_buf()) }
fn within(root:&Path,target:&Path)->bool { canonical(target).strip_prefix(canonical(root)).is_ok_and(|relative|!relative.as_os_str().is_empty()) }

pub fn resolve_memory_child_launch(command:Option<&str>,prefix_args:Option<&[String]>,env:&BTreeMap<String,String>,runtime:&SenpiLaunchRuntime<'_>)->Result<Launcher,SenpiLaunchError>{
    if let Some(command)=command { return Ok(Launcher{command:command.into(),prefix_args:prefix_args.unwrap_or_default().to_vec()}); }
    resolve_senpi_launch(env,runtime)
}

pub fn without_foreign_package_dir_env(env:&BTreeMap<String,String>,launch:&Launcher)->BTreeMap<String,String>{
    let target=launch.prefix_args.first().map(Path::new).unwrap_or_else(||Path::new(&launch.command));
    env.iter().filter(|(name,root)|!PACKAGE_DIR_NAMES.contains(&name.as_str())||root.trim().is_empty()||within(Path::new(root),target)).map(|(key,value)|(key.clone(),value.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn installed_cli_retains_interpreter() { let root = tempfile::tempdir().unwrap(); let cli = root.path().join("cli.js"); std::fs::write(&cli, "").unwrap(); let lookup = || Some(cli.to_string_lossy().into_owned()); let launcher = resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, is_compiled_engine:false, exec_path: "node", platform: "linux", argv: &[], resolve_installed_cli: &lookup }).unwrap(); assert_eq!(launcher.command, "node"); assert_eq!(launcher.prefix_args, [cli.to_string_lossy()]); }
    #[test] fn current_absolute_entry_is_fallback() { let root = tempfile::tempdir().unwrap(); let entry = root.path().join("cli.js"); std::fs::write(&entry, "").unwrap(); let argv = vec!["node".into(), entry.to_string_lossy().into_owned()]; let launcher = resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, is_compiled_engine: false, exec_path: "node", platform: "linux", argv: &argv, resolve_installed_cli: &|| None }).unwrap(); assert_eq!(launcher.prefix_args, [argv[1].clone()]); }
    #[test] fn missing_executable_never_launches_bare_interpreter() { assert!(resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, is_compiled_engine: false, exec_path: "node", platform: "linux", argv: &[], resolve_installed_cli: &|| None }).is_err()); }
    #[test] fn windows_npm_shim_resolves_adjacent_cli() { let root = tempfile::tempdir().unwrap(); let shim = root.path().join("senpi.cmd"); let cli = root.path().join("node_modules/@code-yeongyu/senpi/dist/cli.js"); std::fs::create_dir_all(cli.parent().unwrap()).unwrap(); std::fs::write(&shim, "@echo off\r\n").unwrap(); std::fs::write(&cli, "").unwrap(); let env = BTreeMap::from([("PATH".into(), "".into()), ("SENPI_BIN".into(), shim.to_string_lossy().into_owned())]); let launcher = resolve_senpi_launch(&env, &SenpiLaunchRuntime { is_bun_binary: false, is_compiled_engine: false, exec_path: "C:\\node.exe", platform: "win32", argv: &[], resolve_installed_cli: &|| None }).unwrap(); assert_eq!(launcher.command, "C:\\node.exe"); assert_eq!(launcher.prefix_args, [cli.canonicalize().unwrap().to_string_lossy()]); }
    #[test] fn restricted_path_launches_real_script() {
        let root = tempfile::tempdir().unwrap();
        let (entry, content, exec_path, platform) = if cfg!(windows) {
            (root.path().join("entry.js"), "console.log('LAUNCH_OK');\n", "node", "win32")
        } else {
            (root.path().join("entry.sh"), "printf 'LAUNCH_OK\\n'\n", "/bin/sh", "linux")
        };
        std::fs::write(&entry, content).unwrap();
        let lookup = || Some(entry.to_string_lossy().into_owned());
        let launcher = resolve_senpi_launch(&BTreeMap::from([("PATH".into(), "".into())]), &SenpiLaunchRuntime { is_bun_binary: false, is_compiled_engine: false, exec_path, platform, argv: &[], resolve_installed_cli: &lookup }).unwrap();
        let mut command = std::process::Command::new(launcher.command);
        command.args(launcher.prefix_args).env_clear();
        #[cfg(windows)]
        for key in ["SystemRoot", "PATH", "PATHEXT"] {
            if let Some(val) = std::env::var_os(key) {
                command.env(key, val);
            }
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "LAUNCH_OK");
    }
    #[test] fn explicit_memory_launch_preserves_prefix_and_filters_foreign_roots(){let root=tempfile::tempdir().unwrap();let foreign=root.path().join("other");let own=root.path().join("own");std::fs::create_dir_all(&foreign).unwrap();std::fs::create_dir_all(&own).unwrap();let script=own.join("cli.js");std::fs::write(&script,"").unwrap();let prefix=vec![script.to_string_lossy().into_owned(),"--engine".into()];let launcher=resolve_memory_child_launch(Some("node"),Some(&prefix),&BTreeMap::new(),&SenpiLaunchRuntime{is_bun_binary:false,is_compiled_engine:false,exec_path:"node",platform:"linux",argv:&[],resolve_installed_cli:&||None}).unwrap();assert_eq!(launcher.prefix_args,prefix);let env=BTreeMap::from([("OMO_PACKAGE_DIR".into(),foreign.to_string_lossy().into_owned()),("SENPI_PACKAGE_DIR".into(),own.to_string_lossy().into_owned()),("OTHER".into(),"keep".into())]);let cleaned=without_foreign_package_dir_env(&env,&launcher);assert!(!cleaned.contains_key("OMO_PACKAGE_DIR"));assert!(cleaned.contains_key("SENPI_PACKAGE_DIR"));assert_eq!(cleaned["OTHER"],"keep");}
}
