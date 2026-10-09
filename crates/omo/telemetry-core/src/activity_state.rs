use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use utils::atomic_write::write_file_atomically;
use utils::xdg_data_dir::ResolveXdgDataDirOptions;
use utils::xdg_data_dir::XdgOsProvider;
use utils::xdg_data_dir::resolve_xdg_data_dir;

use crate::day_claim::DayClaimOutcome;
use crate::day_claim::claim_utc_day;
use crate::diagnostics::to_iso_string;
use crate::types::TelemetryDiagnosticEvent;
use crate::types::TelemetryDiagnosticInput;
use crate::types::TelemetryEnv;
use crate::types::TelemetryError;

const POSTHOG_ACTIVITY_STATE_FILE: &str = "posthog-activity.json";
const LAST_ACTIVE_DAY_KEY: &str = "lastActiveDayUTC";
const DIAGNOSTIC_SOURCE: &str = "shared";

// Bounds a process that cannot persist its stamp at all: without this it would ask again on every
// call, which is how a reload loop turns one machine into thousands of events a day.
fn captured_days_by_state_dir() -> &'static Mutex<HashMap<PathBuf, String>> {
    static CAPTURED: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    CAPTURED.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostHogActivityCaptureState {
    #[serde(rename = "dayUTC")]
    pub day_utc: String,
    pub capture_daily: bool,
}

#[derive(Clone, Copy)]
pub struct DailyActiveCaptureStateInput<'a> {
    pub state_dir: &'a Path,
    pub now: Option<DateTime<Utc>>,
    pub diagnostics: Option<&'a dyn Fn(&TelemetryDiagnosticInput)>,
}

#[derive(Clone, Copy, Default)]
pub struct ResolveTelemetryStateDirOptions<'a> {
    pub env: Option<&'a TelemetryEnv>,
    pub os_provider: Option<&'a dyn XdgOsProvider>,
}

pub fn resolve_telemetry_state_dir(
    cache_dir_name: &str,
    options: &ResolveTelemetryStateDirOptions<'_>,
) -> PathBuf {
    let data_dir = resolve_xdg_data_dir(
        cache_dir_name,
        &ResolveXdgDataDirOptions {
            env: options.env,
            os_provider: options.os_provider,
        },
    );
    let xdg_state_dir = options
        .env
        .and_then(|env| env.get("XDG_DATA_HOME"))
        .map(|xdg| Path::new(xdg).join(cache_dir_name));

    let is_product_dir = match &xdg_state_dir {
        Some(xdg_state_dir) => &data_dir == xdg_state_dir,
        None => data_dir
            .file_name()
            .is_some_and(|name| name == cache_dir_name),
    };
    if is_product_dir {
        return data_dir;
    }
    data_dir.join(cache_dir_name)
}

pub fn get_telemetry_activity_state_file_path(state_dir: &Path) -> PathBuf {
    state_dir.join(POSTHOG_ACTIVITY_STATE_FILE)
}

/// Returns whether today's daily-active event is still due, recording today as sent when it is.
pub fn get_daily_active_capture_state(
    input: &DailyActiveCaptureStateInput<'_>,
) -> PostHogActivityCaptureState {
    let iso = to_iso_string(input.now.unwrap_or_else(Utc::now));
    let day_utc = iso[..10].to_string();

    {
        let guard = captured_days_by_state_dir()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.get(input.state_dir) == Some(&day_utc) {
            return PostHogActivityCaptureState {
                day_utc,
                capture_daily: false,
            };
        }
    }

    let state = read_state(input);
    if state.get(LAST_ACTIVE_DAY_KEY).and_then(Value::as_str) == Some(&day_utc) {
        let mut guard = captured_days_by_state_dir()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(input.state_dir.to_path_buf(), day_utc.clone());
        return PostHogActivityCaptureState {
            day_utc,
            capture_daily: false,
        };
    }

    let claim = claim_utc_day(input.state_dir, &day_utc);
    {
        let mut guard = captured_days_by_state_dir()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(input.state_dir.to_path_buf(), day_utc.clone());
    }

    if claim == DayClaimOutcome::AlreadyClaimed {
        return PostHogActivityCaptureState {
            day_utc,
            capture_daily: false,
        };
    }

    // The claim already carries the decision, so the state file is advisory: it keeps the fast path
    // read-only, repairs a corrupt file, and lets a client predating claims keep deduping.
    if claim == DayClaimOutcome::Claimed {
        let mut next_state = state;
        next_state.insert(LAST_ACTIVE_DAY_KEY.into(), day_utc.clone().into());
        write_state(input, &next_state);
    }

    PostHogActivityCaptureState {
        day_utc,
        capture_daily: true,
    }
}

fn report(
    input: &DailyActiveCaptureStateInput<'_>,
    event: TelemetryDiagnosticEvent,
    error: String,
) {
    if let Some(diagnostics) = input.diagnostics {
        diagnostics(&TelemetryDiagnosticInput::failure(
            event,
            DIAGNOSTIC_SOURCE,
            TelemetryError::new(error),
        ));
    }
}

fn read_state(input: &DailyActiveCaptureStateInput<'_>) -> Map<String, Value> {
    let path = get_telemetry_activity_state_file_path(input.state_dir);
    if !path.exists() {
        return Map::new();
    }
    let parsed = fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|content| {
            serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())
        });
    match parsed {
        Ok(Value::Object(state)) => state,
        Ok(_) => Map::new(),
        Err(error) => {
            report(
                input,
                TelemetryDiagnosticEvent::TelemetryActivityStateReadFailed,
                error,
            );
            Map::new()
        }
    }
}

fn write_state(input: &DailyActiveCaptureStateInput<'_>, state: &Map<String, Value>) {
    let path = get_telemetry_activity_state_file_path(input.state_dir);
    let written = fs::create_dir_all(input.state_dir)
        .map_err(|error| error.to_string())
        .and_then(|()| serde_json::to_string_pretty(state).map_err(|error| error.to_string()))
        .and_then(|json| {
            write_file_atomically(&path, &format!("{json}
")).map_err(|error| error.to_string())
        });
    if let Err(error) = written {
        report(
            input,
            TelemetryDiagnosticEvent::TelemetryActivityStateWriteFailed,
            error,
        );
    }
}
