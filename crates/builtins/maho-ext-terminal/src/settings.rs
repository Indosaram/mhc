use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutAction {
    Background,
    Kill,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyMode {
    Wake,
    NextTurn,
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorDeliverySettings {
    pub coalesce_window_ms: f64,
    pub rate_limit_ms: f64,
    pub max_lines_per_injection: f64,
    pub max_chars_per_injection: f64,
    pub wake_budget: f64,
    /// Average wakes an hour (and burst size) of a persistent command watch; more lines batch.
    pub wakes_per_hour: f64,
}

/// Runaway guard of a persistent command watch: pause after this many lines a minute for this many minutes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorRunawayGuard {
    pub lines_per_minute: f64,
    pub minutes: f64,
}
/// Upper bound of a wake rate (setting or per-monitor override): one wake a second on average.
pub const MAX_MONITOR_WAKES_PER_HOUR: f64 = 3600.0;

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTerminalSettings {
    pub default_cols: f64,
    pub default_rows: f64,
    pub scrollback: f64,
    pub max_sessions: f64,
    pub timeout_action: TimeoutAction,
    pub notify: NotifyMode,
    pub monitor: MonitorDeliverySettings,
    pub monitor_runaway: MonitorRunawayGuard,
    pub max_durable_monitors: Option<usize>,
}

pub const TERMINAL_SETTINGS_DEFAULTS: ResolvedTerminalSettings = ResolvedTerminalSettings {
    default_cols: 120.0,
    default_rows: 40.0,
    scrollback: 10_000.0,
    max_sessions: 32.0,
    timeout_action: TimeoutAction::Background,
    notify: NotifyMode::Wake,
    monitor: MonitorDeliverySettings {
        coalesce_window_ms: 2000.0,
        rate_limit_ms: 5000.0,
        max_lines_per_injection: 50.0,
        max_chars_per_injection: 4096.0,
        wake_budget: 5.0,
        wakes_per_hour: crate::shared::DEFAULT_MONITOR_WAKES_PER_HOUR as f64,
    },
    monitor_runaway: MonitorRunawayGuard {
        lines_per_minute: crate::shared::DEFAULT_MONITOR_RUNAWAY_LINES_PER_MINUTE as f64,
        minutes: crate::shared::DEFAULT_MONITOR_RUNAWAY_MINUTES as f64,
    },
    max_durable_monitors: None,
};

fn positive_int(value: Option<&Value>, fallback: f64, maximum: f64) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 1.0)
        .map(|v| v.trunc().min(maximum))
        .unwrap_or(fallback)
}

fn non_negative_int(value: Option<&Value>, fallback: f64) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(f64::trunc)
        .unwrap_or(fallback)
}

pub fn resolve_terminal_settings(raw: Option<&Value>) -> ResolvedTerminalSettings {
    let Some(raw) = raw else {
        return TERMINAL_SETTINGS_DEFAULTS;
    };
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let defaults = &TERMINAL_SETTINGS_DEFAULTS;
    ResolvedTerminalSettings {
        default_cols: positive_int(
            raw.get("defaultCols"),
            defaults.default_cols,
            MAX_SAFE_INTEGER,
        ),
        default_rows: positive_int(
            raw.get("defaultRows"),
            defaults.default_rows,
            MAX_SAFE_INTEGER,
        ),
        scrollback: non_negative_int(raw.get("scrollback"), defaults.scrollback),
        max_sessions: positive_int(
            raw.get("maxSessions"),
            defaults.max_sessions,
            MAX_SAFE_INTEGER,
        ),
        timeout_action: match raw.get("timeoutAction").and_then(Value::as_str) {
            Some("kill") => TimeoutAction::Kill,
            _ => TimeoutAction::Background,
        },
        notify: match raw.get("notify").and_then(Value::as_str) {
            Some("next-turn") => NotifyMode::NextTurn,
            Some("off") => NotifyMode::Off,
            _ => NotifyMode::Wake,
        },
        monitor: MonitorDeliverySettings {
            coalesce_window_ms: positive_int(
                raw.get("monitorCoalesceWindowMs"),
                defaults.monitor.coalesce_window_ms,
                60_000.0,
            ),
            rate_limit_ms: positive_int(
                raw.get("monitorRateLimitMs"),
                defaults.monitor.rate_limit_ms,
                3_600_000.0,
            ),
            max_lines_per_injection: positive_int(
                raw.get("monitorMaxLinesPerInjection"),
                defaults.monitor.max_lines_per_injection,
                200.0,
            ),
            max_chars_per_injection: positive_int(
                raw.get("monitorMaxCharsPerInjection"),
                defaults.monitor.max_chars_per_injection,
                16_384.0,
            ),
            wake_budget: positive_int(
                raw.get("monitorWakeBudget"),
                defaults.monitor.wake_budget,
                100.0,
            ),
            wakes_per_hour: positive_int(
                raw.get("monitorWakesPerHour"),
                defaults.monitor.wakes_per_hour,
                MAX_MONITOR_WAKES_PER_HOUR,
            ),
        },
        monitor_runaway: MonitorRunawayGuard {
            lines_per_minute: positive_int(
                raw.get("monitorRunawayLinesPerMinute"),
                defaults.monitor_runaway.lines_per_minute,
                MAX_SAFE_INTEGER,
            ),
            minutes: positive_int(
                raw.get("monitorRunawayMinutes"),
                defaults.monitor_runaway.minutes,
                1440.0,
            ),
        },
        max_durable_monitors: raw
            .get("maxDurableMonitors")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 1.0)
            .map(|value| value.trunc() as usize),
    }
}

pub fn load_terminal_settings(
    global: Option<&Value>,
    project: Option<&Value>,
) -> ResolvedTerminalSettings {
    let mut merged = serde_json::Map::new();
    for layer in [global, project].into_iter().flatten() {
        if let Some(layer) = layer.as_object() {
            merged.extend(layer.clone());
        }
    }
    resolve_terminal_settings(Some(&Value::Object(merged)))
}

pub fn load_session_settings(
    ctx: &maho_ext_api::types::ExtensionContext,
) -> Result<(ResolvedTerminalSettings, Option<String>), maho_ext_api::types::ExtensionFailure> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|error| maho_ext_api::types::ExtensionFailure::new(error.to_string()))?;
    let settings = maho_core::settings_manager::SettingsManager::create(
        &ctx.cwd.to_string_lossy(),
        &ctx.agent_dir.to_string_lossy(),
        &home,
        ctx.is_project_trusted(),
    );
    let terminal = load_terminal_settings(
        settings.get_global().get("terminal"),
        settings.get_project().get("terminal"),
    );
    let shell = settings.get_shell_path();
    Ok((terminal, shell))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_block_fills_defaults() {
        assert_eq!(resolve_terminal_settings(None), TERMINAL_SETTINGS_DEFAULTS);
    }

    #[test]
    fn valid_values_override_invalid_values_default() {
        let result = resolve_terminal_settings(Some(
            &json!({"defaultCols":200,"maxSessions":0,"notify":"off","timeoutAction":"bogus"}),
        ));
        assert_eq!(result.default_cols, 200.0);
        assert_eq!(result.max_sessions, 32.0);
        assert_eq!(result.notify, NotifyMode::Off);
        assert_eq!(result.timeout_action, TimeoutAction::Background);
    }

    #[test]
    fn monitor_delivery_settings_are_bounded() {
        let result = resolve_terminal_settings(Some(
            &json!({"monitorCoalesceWindowMs":1200,"monitorRateLimitMs":7000,"monitorMaxLinesPerInjection":75,"monitorMaxCharsPerInjection":8000,"monitorWakeBudget":9}),
        ));
        assert_eq!(
            result.monitor,
            MonitorDeliverySettings {
                coalesce_window_ms: 1200.0,
                rate_limit_ms: 7000.0,
                max_lines_per_injection: 75.0,
                max_chars_per_injection: 8000.0,
                wake_budget: 9.0,
                wakes_per_hour: 12.0
            }
        );
        assert_eq!(
            resolve_terminal_settings(Some(&json!({"monitorMaxLinesPerInjection":0})))
                .monitor
                .max_lines_per_injection,
            50.0
        );
        assert_eq!(
            resolve_terminal_settings(Some(&json!({"monitorWakeBudget":101})))
                .monitor
                .wake_budget,
            100.0
        );
    }

    #[test]
    fn wake_pacing_and_runaway_guard_are_bounded() {
        assert_eq!(TERMINAL_SETTINGS_DEFAULTS.monitor.wakes_per_hour, 12.0);
        assert_eq!(
            TERMINAL_SETTINGS_DEFAULTS.monitor_runaway,
            MonitorRunawayGuard {
                lines_per_minute: 120.0,
                minutes: 5.0
            }
        );
        let result = resolve_terminal_settings(Some(
            &json!({"monitorWakesPerHour":99999,"monitorRunawayLinesPerMinute":300,"monitorRunawayMinutes":5000}),
        ));
        assert_eq!(result.monitor.wakes_per_hour, 3600.0);
        assert_eq!(
            result.monitor_runaway,
            MonitorRunawayGuard {
                lines_per_minute: 300.0,
                minutes: 1440.0
            }
        );
        let result = resolve_terminal_settings(Some(
            &json!({"monitorWakesPerHour":0,"monitorRunawayMinutes":0}),
        ));
        assert_eq!(result.monitor.wakes_per_hour, 12.0);
        assert_eq!(result.monitor_runaway.minutes, 5.0);
    }

    #[test]
    fn project_layer_overrides_global_and_fractions_truncate() {
        let result = load_terminal_settings(
            Some(&json!({"defaultCols":200,"scrollback":0})),
            Some(&json!({"defaultCols":123.9,"notify":"next-turn"})),
        );
        assert_eq!(result.default_cols, 123.0);
        assert_eq!(result.scrollback, 0.0);
        assert_eq!(result.notify, NotifyMode::NextTurn);
    }

    #[test]
    fn durable_monitor_cap_is_optional_and_positive_only() {
        assert_eq!(
            resolve_terminal_settings(Some(&json!({"maxDurableMonitors":3.9})))
                .max_durable_monitors,
            Some(3)
        );
        assert_eq!(
            resolve_terminal_settings(Some(&json!({"maxDurableMonitors":0}))).max_durable_monitors,
            None
        );
        assert_eq!(TERMINAL_SETTINGS_DEFAULTS.max_durable_monitors, None);
    }
}
