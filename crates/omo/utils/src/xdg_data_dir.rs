//! XDG data directory resolution with a writable tmp fallback.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Source of home and tmp directories (injectable for tests and sandboxed callers).
pub trait XdgOsProvider {
    fn homedir(&self) -> PathBuf;
    fn tmpdir(&self) -> PathBuf;
}

/// The real OS directories.
pub struct SystemOsProvider;

impl XdgOsProvider for SystemOsProvider {
    fn homedir(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_default()
    }

    fn tmpdir(&self) -> PathBuf {
        std::env::temp_dir()
    }
}

#[derive(Default)]
pub struct ResolveXdgDataDirOptions<'a> {
    /// Environment to read `XDG_DATA_HOME` from; `None` reads the process environment.
    pub env: Option<&'a HashMap<String, String>>,
    pub os_provider: Option<&'a dyn XdgOsProvider>,
}

pub fn resolve_xdg_data_dir(app_name: &str, options: &ResolveXdgDataDirOptions<'_>) -> PathBuf {
    let os_provider = options.os_provider.unwrap_or(&SystemOsProvider);
    let xdg = match options.env {
        Some(env) => env.get("XDG_DATA_HOME").cloned(),
        None => std::env::var("XDG_DATA_HOME").ok(),
    };
    let preferred = xdg.map_or_else(
        || os_provider.homedir().join(".local").join("share"),
        PathBuf::from,
    );
    if is_writable_dir(&preferred) {
        return preferred;
    }
    let fallback = os_provider.tmpdir().join(format!("{app_name}-data"));
    // The TS original lets this mkdir throw; a failing tmp dir is surfaced by the caller's first write.
    let _ = fs::create_dir_all(&fallback);
    fallback
}

fn is_writable_dir(path: &Path) -> bool {
    fs::create_dir_all(path).is_ok()
        && fs::metadata(path).is_ok_and(|meta| meta.is_dir() && !meta.permissions().readonly())
}
