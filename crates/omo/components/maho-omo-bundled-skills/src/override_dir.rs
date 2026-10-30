//! Port of `omo-senpi/src/components/bundled-skills/override.ts` at omo v5.1.52.
//!
//! `OMO_BUNDLED_SKILLS_DIR` is valid only when it is an absolute, readable, listable directory.
//! Every bundled-skill consumer calls [`resolve_bundled_skills_override`], so an invalid value is
//! logged once per process and shown once through the UI when one is given; callers then fall back
//! to the packaged skills.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use maho_ext_api::{ComponentLogger, ExtensionUi, NotificationType};

pub const BUNDLED_SKILLS_OVERRIDE_ENV: &str = "OMO_BUNDLED_SKILLS_DIR";

/// Upstream keeps the ledger on `globalThis` because three bundles each carry the module; one
/// process-wide set is the native equivalent.
fn seen() -> &'static Mutex<BTreeSet<String>> {
    static SEEN: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn once(key: &str, message: &str) -> bool {
    seen()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(format!("{key}{message}"))
}

/// The errno-style code upstream prints for a directory that cannot be listed.
fn problem_code(path: &Path, error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_owned(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
        _ if path.is_file() => "ENOTDIR".to_owned(),
        _ => error
            .raw_os_error()
            .map_or_else(|| "EIO".to_owned(), |code| format!("os error {code}")),
    }
}

/// A valid `OMO_BUNDLED_SKILLS_DIR`, or `None` when it is absent, empty or invalid.
pub fn resolve_bundled_skills_override(
    env: &BTreeMap<String, String>,
    logger: Option<&dyn ComponentLogger>,
    ui: Option<&dyn ExtensionUi>,
) -> Option<PathBuf> {
    let value = env
        .get(BUNDLED_SKILLS_OVERRIDE_ENV)
        .filter(|value| !value.is_empty())?;
    let path = Path::new(value);
    let problem = if path.is_absolute() {
        match std::fs::read_dir(path) {
            Ok(_) => return Some(path.to_path_buf()),
            Err(error) => problem_code(path, &error),
        }
    } else {
        "relative".to_owned()
    };
    let message = format!(
        "{BUNDLED_SKILLS_OVERRIDE_ENV}={value} ignored ({problem}); needs a readable absolute dir"
    );
    if once("log", &message) {
        match logger {
            Some(logger) => logger.error(&message, None),
            None => maho_ext_api::diagnostic(&message),
        }
    }
    if let Some(ui) = ui
        && once("ui", &message)
    {
        ui.notify(&message, NotificationType::Error);
    }
    None
}
