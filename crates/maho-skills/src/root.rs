//! The bundled skill root (spec §2, delta D1/D2).
//!
//! One resolver serves every consumer (`/skill`, the `skill` tool, `resources_discover`, and
//! callers without a UI) so the override rules and the once-per-process notice apply everywhere.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Environment variable naming the bundled skill root.
pub const SKILLS_ROOT_ENV: &str = "MAHO_SKILLS_ROOT";
/// The launcher's legacy name, read only when [`SKILLS_ROOT_ENV`] is unset or empty.
pub const LEGACY_SKILLS_ROOT_ENV: &str = "OMO_BUNDLED_SKILLS_DIR";

/// Why an override value cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvalidRoot {
    /// Not an absolute path.
    Relative,
    /// The directory cannot be opened and listed; the OS error name (`ENOENT`, `EACCES`,
    /// `ENOTDIR`, ...) or the error text when it has no well-known name.
    Os(String),
}

impl InvalidRoot {
    pub fn reason(&self) -> &str {
        match self {
            Self::Relative => "relative",
            Self::Os(name) => name,
        }
    }
}

fn os_error_name(error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => "ENOENT".into(),
        ErrorKind::PermissionDenied => "EACCES".into(),
        ErrorKind::NotADirectory => "ENOTDIR".into(),
        _ => error.to_string(),
    }
}

/// Checks an override value: absolute, and a directory this process can open and list.
pub fn validate_root(value: &Path) -> Result<(), InvalidRoot> {
    if !value.is_absolute() {
        return Err(InvalidRoot::Relative);
    }
    let mut entries =
        std::fs::read_dir(value).map_err(|error| InvalidRoot::Os(os_error_name(&error)))?;
    match entries.next() {
        Some(Err(error)) => Err(InvalidRoot::Os(os_error_name(&error))),
        _ => Ok(()),
    }
}

/// The notice for an invalid override value: variable, value, reason, what a valid value is.
pub fn invalid_root_message(variable: &str, value: &str, invalid: &InvalidRoot) -> String {
    format!(
        "{variable}=\"{value}\" is ignored ({}): it must be an absolute path to a directory mhc can read. Using the skills beside the executable instead.",
        invalid.reason()
    )
}

/// Where a resolver reports an invalid override. `ui` is `None` for consumers without a UI.
pub struct NoticeContext<'a> {
    pub log: &'a dyn Fn(&str),
    pub ui: Option<&'a dyn Fn(&str)>,
}

fn diagnostic_log(message: &str) {
    maho_ext_api::diagnostic(&format!("skills: error: {message}"));
}

impl NoticeContext<'static> {
    /// Logs through the process diagnostic sink; no UI.
    pub fn diagnostic() -> Self {
        Self {
            log: &diagnostic_log,
            ui: None,
        }
    }
}

/// Messages already reported, per channel; each distinct message is reported at most once.
#[derive(Debug, Default)]
pub struct NoticeLedger {
    logged: Mutex<HashSet<String>>,
    shown: Mutex<HashSet<String>>,
}

impl NoticeLedger {
    /// The ledger of this process.
    pub fn process() -> Arc<Self> {
        static LEDGER: OnceLock<Arc<NoticeLedger>> = OnceLock::new();
        Arc::clone(LEDGER.get_or_init(|| Arc::new(Self::default())))
    }

    fn first(set: &Mutex<HashSet<String>>, message: &str) -> bool {
        set.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(message.to_owned())
    }

    fn report(&self, message: &str, notices: &NoticeContext<'_>) {
        if Self::first(&self.logged, message) {
            (notices.log)(message);
        }
        if let Some(ui) = notices.ui
            && Self::first(&self.shown, message)
        {
            ui(message);
        }
    }
}

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Resolves the bundled root from the environment and the executable location.
#[derive(Clone)]
pub struct RootResolver {
    pub env: EnvLookup,
    /// The running executable; its sibling `skills` directory is the fallback.
    pub exe: Option<PathBuf>,
    pub ledger: Arc<NoticeLedger>,
}

impl std::fmt::Debug for RootResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootResolver")
            .field("exe", &self.exe)
            .finish_non_exhaustive()
    }
}

impl Default for RootResolver {
    /// The process environment, `current_exe`, and the process ledger.
    fn default() -> Self {
        Self {
            env: Arc::new(|key: &str| std::env::var(key).ok()),
            exe: std::env::current_exe().ok(),
            ledger: NoticeLedger::process(),
        }
    }
}

impl RootResolver {
    /// The override that is read: `(variable, value)` of [`SKILLS_ROOT_ENV`] when non-empty,
    /// else of [`LEGACY_SKILLS_ROOT_ENV`] when non-empty.
    pub fn override_value(&self) -> Option<(&'static str, String)> {
        [SKILLS_ROOT_ENV, LEGACY_SKILLS_ROOT_ENV]
            .into_iter()
            .find_map(|key| {
                (self.env)(key)
                    .filter(|value| !value.is_empty())
                    .map(|value| (key, value))
            })
    }

    /// The bundled root. `option` (a directory handed in by code) always wins and is never
    /// reported. Otherwise a valid override is used; an invalid one is reported through
    /// `notices` (once per distinct message) and the executable sibling is used.
    pub fn resolve(&self, option: Option<&Path>, notices: &NoticeContext<'_>) -> Option<PathBuf> {
        if let Some(option) = option {
            return Some(option.to_path_buf());
        }
        if let Some((variable, value)) = self.override_value() {
            let path = PathBuf::from(&value);
            match validate_root(&path) {
                Ok(()) => return Some(path),
                Err(invalid) => self
                    .ledger
                    .report(&invalid_root_message(variable, &value, &invalid), notices),
            }
        }
        sibling_root(self.exe.as_deref())
    }
}

fn sibling_root(exe: Option<&Path>) -> Option<PathBuf> {
    let sibling = exe?.parent()?.join("skills");
    sibling.is_dir().then_some(sibling)
}

/// The bundled skill root for this process, for consumers without a UI (an invalid override is
/// logged once through the diagnostic sink).
pub fn bundled_skills_root() -> Option<PathBuf> {
    RootResolver::default().resolve(None, &NoticeContext::diagnostic())
}

/// The root from one already-read override value and the executable path, without notices: a
/// valid (absolute, listable directory) `env` value, else `skills/` beside `exe` when it exists.
pub fn bundled_skills_root_from(env: Option<&Path>, exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = env.filter(|root| !root.as_os_str().is_empty())
        && validate_root(root).is_ok()
    {
        return Some(root.to_path_buf());
    }
    sibling_root(exe)
}
