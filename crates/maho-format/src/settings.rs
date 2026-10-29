//! Spec §4: the `formatOnMutation` keys.
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_048_576;
pub const DEFAULT_TIMEOUT_MS: u64 = 3000;
pub const SECTION: &str = "formatOnMutation";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    Off,
    #[default]
    BestEffort,
    Required,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "off" => Some(Self::Off),
            "best-effort" => Some(Self::BestEffort),
            "required" => Some(Self::Required),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    /// Language name -> enabled; `false` turns formatting off for that language.
    pub languages: BTreeMap<String, bool>,
    pub max_file_bytes: u64,
    pub timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::BestEffort,
            languages: BTreeMap::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

fn positive(object: &Map<String, Value>, keys: [&str; 2]) -> Result<Option<u64>, ()> {
    match keys.iter().find_map(|key| object.get(*key)) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|number| *number > 0)
            .map(Some)
            .ok_or(()),
    }
}

fn parse_section(value: &Value) -> Result<Settings, ()> {
    let object = value.as_object().ok_or(())?;
    let mut settings = Settings::default();
    if let Some(mode) = object.get("mode") {
        settings.mode = mode.as_str().and_then(Mode::parse).ok_or(())?;
    }
    if let Some(languages) = object.get("languages") {
        for (name, enabled) in languages.as_object().ok_or(())? {
            settings
                .languages
                .insert(name.clone(), enabled.as_bool().ok_or(())?);
        }
    }
    if let Some(bytes) = positive(object, ["maxFileBytes", "max_file_bytes"])? {
        settings.max_file_bytes = bytes;
    }
    if let Some(ms) = positive(object, ["timeoutMs", "timeout_ms"])? {
        settings.timeout = Duration::from_millis(ms);
    }
    Ok(settings)
}

impl Settings {
    /// From a `formatOnMutation` value; absent or invalid values give the defaults.
    pub fn from_section(value: Option<&Value>) -> Self {
        value
            .and_then(|value| parse_section(value).ok())
            .unwrap_or_default()
    }

    /// From a merged config view (the object holding `formatOnMutation`).
    pub fn from_view(view: &Value) -> Self {
        Self::from_section(view.get(SECTION))
    }

    /// The merged native-harness config of `cwd` (top level, `[native]` block, active profile).
    pub fn load(cwd: &Path) -> Self {
        let env = maho_config::process_env();
        Self::from_view(&maho_config::load_native(cwd, &env).view)
    }

    /// The effective mode for `language`.
    pub fn mode_for(&self, language: &str) -> Mode {
        if self.mode == Mode::Off || self.languages.get(language) == Some(&false) {
            Mode::Off
        } else {
            self.mode
        }
    }
}

/// Language name of `path`: `.ts`/`.tsx` -> `typescript`, `.py` -> `python`, else the bare
/// extension (empty when there is none).
pub fn language_of(path: &Path) -> String {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    match extension.as_str() {
        "ts" | "tsx" => "typescript".to_owned(),
        "py" => "python".to_owned(),
        _ => extension,
    }
}
