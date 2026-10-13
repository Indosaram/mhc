use std::{env, path::PathBuf};

pub fn node() -> String {
    env::split_paths(&env::var_os("PATH").expect("PATH must be set to locate node"))
        .map(|dir| dir.join("node"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("could not find executable node on PATH"))
        .to_string_lossy()
        .into_owned()
}

pub fn senpi(rel: &str) -> String {
    let root = env::var_os("SENPI_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").expect("HOME must be set")).join("code/senpi"));
    root.join(rel).to_string_lossy().into_owned()
}
