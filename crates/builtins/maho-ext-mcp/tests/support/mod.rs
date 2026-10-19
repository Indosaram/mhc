use std::{env, path::PathBuf};

#[allow(dead_code)]
pub fn node() -> String {
    let binary = if cfg!(windows) { "node.exe" } else { "node" };
    env::split_paths(&env::var_os("PATH").expect("PATH must be set to locate node"))
        .map(|dir| dir.join(binary))
        .find(|path| path.is_file())
        .or_else(|| {
            if cfg!(windows) {
                env::split_paths(&env::var_os("PATH").unwrap_or_default())
                    .map(|dir| dir.join("node"))
                    .find(|path| path.is_file())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("could not find executable node on PATH"))
        .to_string_lossy()
        .into_owned()
}

#[allow(dead_code)]
pub fn senpi(rel: &str) -> String {
    let root = env::var_os("SENPI_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .expect("HOME or USERPROFILE must be set");
            PathBuf::from(home).join("code/senpi")
        });
    root.join(rel).to_string_lossy().into_owned()
}
