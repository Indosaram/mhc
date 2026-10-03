use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_HARD_LIMIT_SECONDS: f64 = 1800.0;
pub const DEFAULT_FOREGROUND_WINDOW_SECONDS: f64 = 60.0;
pub const DEFAULT_RUN_BUDGET_SECONDS: f64 = 300.0;
pub const DEFAULT_MAX_DETACHED_CELLS: f64 = 15.0;
pub const HARD_LIMIT_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_HARD_LIMIT_SECONDS";
pub const FOREGROUND_WINDOW_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_FOREGROUND_SECONDS";
pub const RUN_BUDGET_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_RUN_BUDGET_SECONDS";
pub const MAX_DETACHED_CELLS_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_MAX_DETACHED_CELLS";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Languages {
    pub py: bool,
    pub js: bool,
    pub rb: bool,
    pub jl: bool,
}

impl Default for Languages {
    fn default() -> Self { Self { py: true, js: true, rb: false, jl: false } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodemodeTaskTools {
    pub task: String,
    pub output: String,
}

impl Default for CodemodeTaskTools {
    fn default() -> Self { Self { task: "task".into(), output: "task_output".into() } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CodemodeOutputSink {
    pub head_bytes: f64,
    pub max_columns: f64,
}

impl Default for CodemodeOutputSink {
    fn default() -> Self { Self { head_bytes: 20_480.0, max_columns: 768.0 } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CodemodeSettings {
    pub languages: Languages,
    pub cell_timeout_seconds: f64,
    pub foreground_window_seconds: f64,
    pub run_budget_seconds: f64,
    pub hard_limit_seconds: f64,
    pub max_detached_cells: f64,
    pub parallel_pool_width: f64,
    pub task_tools: CodemodeTaskTools,
    pub output_sink: CodemodeOutputSink,
    pub status_events: bool,
}

impl Default for CodemodeSettings {
    fn default() -> Self {
        Self {
            languages: Languages::default(), cell_timeout_seconds: 30.0,
            foreground_window_seconds: DEFAULT_FOREGROUND_WINDOW_SECONDS,
            run_budget_seconds: DEFAULT_RUN_BUDGET_SECONDS,
            hard_limit_seconds: DEFAULT_HARD_LIMIT_SECONDS,
            max_detached_cells: DEFAULT_MAX_DETACHED_CELLS, parallel_pool_width: 4.0,
            task_tools: CodemodeTaskTools::default(), output_sink: CodemodeOutputSink::default(), status_events: true,
        }
    }
}

#[derive(Debug)]
pub struct LoadedCodemodeSettings {
    pub settings: CodemodeSettings,
    pub source: Option<PathBuf>,
    pub warnings: Vec<String>,
}

pub async fn load_codemode_settings(cwd: &Path, home_dir: &Path) -> Result<LoadedCodemodeSettings, std::io::Error> {
    // The native user configuration home is .maho; project artifacts remain .omo.
    let candidates = [cwd.join(".maho/codemode.json"), home_dir.join(".maho/agent/codemode.json")];
    for candidate in candidates {
        if tokio::fs::metadata(&candidate).await.is_err() { continue; }
        let raw = tokio::fs::read_to_string(&candidate).await?;
        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(error) => return Ok(LoadedCodemodeSettings {
                settings: CodemodeSettings::default(), source: Some(candidate.clone()),
                warnings: vec![format!("Invalid JSON in {}: {error}. Falling back to codemode defaults.", candidate.display())],
            }),
        };
        let settings = serde_json::from_value::<CodemodeSettings>(parsed).ok().filter(valid_settings);
        return Ok(match settings {
            Some(settings) => LoadedCodemodeSettings { settings, source: Some(candidate), warnings: Vec::new() },
            None => LoadedCodemodeSettings {
                settings: CodemodeSettings::default(), source: Some(candidate.clone()),
                warnings: vec![format!("Invalid codemode settings in {}. Falling back to codemode defaults.", candidate.display())],
            },
        });
    }
    Ok(LoadedCodemodeSettings { settings: CodemodeSettings::default(), source: None, warnings: Vec::new() })
}

fn valid_settings(settings: &CodemodeSettings) -> bool {
    [settings.cell_timeout_seconds, settings.foreground_window_seconds, settings.run_budget_seconds,
     settings.hard_limit_seconds, settings.max_detached_cells, settings.parallel_pool_width]
        .iter().all(|value| value.is_finite() && *value >= 1.0)
        && [settings.output_sink.head_bytes, settings.output_sink.max_columns]
            .iter().all(|value| value.is_finite() && *value >= 0.0)
}

pub type Environment = HashMap<String, String>;

pub fn resolve_enabled_languages(settings: &CodemodeSettings, env: &Environment) -> Languages {
    fn language(value: bool, override_value: Option<&String>) -> bool {
        match override_value.map(|value| value.trim().to_lowercase()).as_deref() {
            Some("0" | "false") => false,
            Some("1" | "true") => true,
            _ => value,
        }
    }
    Languages {
        py: language(settings.languages.py, env.get("SENPI_CODEMODE_PY")),
        js: language(settings.languages.js, env.get("SENPI_CODEMODE_JS")),
        rb: language(settings.languages.rb, env.get("SENPI_CODEMODE_RB")),
        jl: language(settings.languages.jl, env.get("SENPI_CODEMODE_JL")),
    }
}

fn positive_seconds_override(value: Option<&String>) -> Option<f64> {
    let value = value?.trim_start();
    let (sign, digits) = match value.strip_prefix('-') {
        Some(digits) => (-1.0, digits),
        None => (1.0, value.strip_prefix('+').unwrap_or(value)),
    };
    let length = digits.bytes().take_while(u8::is_ascii_digit).count();
    let parsed = digits.get(..length)?.parse::<f64>().ok()? * sign;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed)
}

pub fn resolve_hard_limit_seconds(settings: &CodemodeSettings, env: &Environment) -> f64 {
    positive_seconds_override(env.get(HARD_LIMIT_ENVIRONMENT_FLAG)).unwrap_or(settings.hard_limit_seconds)
}
pub fn resolve_foreground_window_seconds(settings: &CodemodeSettings, env: &Environment) -> f64 {
    positive_seconds_override(env.get(FOREGROUND_WINDOW_ENVIRONMENT_FLAG)).unwrap_or(settings.foreground_window_seconds)
}
pub fn resolve_run_budget_seconds(settings: &CodemodeSettings, env: &Environment) -> f64 {
    positive_seconds_override(env.get(RUN_BUDGET_ENVIRONMENT_FLAG)).unwrap_or(settings.run_budget_seconds)
}
pub fn resolve_max_detached_cells(settings: &CodemodeSettings, env: &Environment) -> f64 {
    positive_seconds_override(env.get(MAX_DETACHED_CELLS_ENVIRONMENT_FLAG)).unwrap_or(settings.max_detached_cells)
}
