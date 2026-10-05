use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutAction { Background, Kill }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyMode { Wake, NextTurn, Off }

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorDeliverySettings {
    pub coalesce_window_ms: f64,
    pub rate_limit_ms: f64,
    pub max_lines_per_injection: f64,
    pub max_chars_per_injection: f64,
    pub wake_budget: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTerminalSettings {
    pub default_cols: f64,
    pub default_rows: f64,
    pub scrollback: f64,
    pub max_sessions: f64,
    pub timeout_action: TimeoutAction,
    pub notify: NotifyMode,
    pub monitor: MonitorDeliverySettings,
}

pub const TERMINAL_SETTINGS_DEFAULTS: ResolvedTerminalSettings = ResolvedTerminalSettings {
    default_cols: 120.0, default_rows: 40.0, scrollback: 10_000.0, max_sessions: 32.0,
    timeout_action: TimeoutAction::Background, notify: NotifyMode::Wake,
    monitor: MonitorDeliverySettings { coalesce_window_ms: 2000.0, rate_limit_ms: 5000.0, max_lines_per_injection: 50.0, max_chars_per_injection: 4096.0, wake_budget: 5.0 },
};

fn positive_int(value: Option<&Value>, fallback: f64, maximum: f64) -> f64 {
    value.and_then(Value::as_f64).filter(|v|v.is_finite() && *v>=1.0).map(|v|v.trunc().min(maximum)).unwrap_or(fallback)
}

fn non_negative_int(value: Option<&Value>, fallback:f64) -> f64 {
    value.and_then(Value::as_f64).filter(|v|v.is_finite() && *v>=0.0).map(f64::trunc).unwrap_or(fallback)
}

pub fn resolve_terminal_settings(raw: Option<&Value>) -> ResolvedTerminalSettings {
    let Some(raw)=raw else {return TERMINAL_SETTINGS_DEFAULTS;};
    const MAX_SAFE_INTEGER:f64=9_007_199_254_740_991.0;
    let defaults=&TERMINAL_SETTINGS_DEFAULTS;
    ResolvedTerminalSettings {
        default_cols:positive_int(raw.get("defaultCols"),defaults.default_cols,MAX_SAFE_INTEGER),
        default_rows:positive_int(raw.get("defaultRows"),defaults.default_rows,MAX_SAFE_INTEGER),
        scrollback:non_negative_int(raw.get("scrollback"),defaults.scrollback),
        max_sessions:positive_int(raw.get("maxSessions"),defaults.max_sessions,MAX_SAFE_INTEGER),
        timeout_action:match raw.get("timeoutAction").and_then(Value::as_str) {Some("kill")=>TimeoutAction::Kill,_=>TimeoutAction::Background},
        notify:match raw.get("notify").and_then(Value::as_str) {Some("next-turn")=>NotifyMode::NextTurn,Some("off")=>NotifyMode::Off,_=>NotifyMode::Wake},
        monitor:MonitorDeliverySettings {
            coalesce_window_ms:positive_int(raw.get("monitorCoalesceWindowMs"),defaults.monitor.coalesce_window_ms,60_000.0),
            rate_limit_ms:positive_int(raw.get("monitorRateLimitMs"),defaults.monitor.rate_limit_ms,3_600_000.0),
            max_lines_per_injection:positive_int(raw.get("monitorMaxLinesPerInjection"),defaults.monitor.max_lines_per_injection,200.0),
            max_chars_per_injection:positive_int(raw.get("monitorMaxCharsPerInjection"),defaults.monitor.max_chars_per_injection,16_384.0),
            wake_budget:positive_int(raw.get("monitorWakeBudget"),defaults.monitor.wake_budget,100.0),
        },
    }
}

pub fn load_terminal_settings(global: Option<&Value>, project: Option<&Value>) -> ResolvedTerminalSettings {
    let mut merged=serde_json::Map::new();
    for layer in [global,project].into_iter().flatten() {if let Some(layer)=layer.as_object() {merged.extend(layer.clone());}}
    resolve_terminal_settings(Some(&Value::Object(merged)))
}

pub fn load_session_settings(ctx:&maho_ext_api::types::ExtensionContext)->Result<(ResolvedTerminalSettings,Option<String>),maho_ext_api::types::ExtensionFailure> {
    let home=std::env::var("HOME").map_err(|error|maho_ext_api::types::ExtensionFailure::new(error.to_string()))?;
    let settings=maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(),&ctx.agent_dir.to_string_lossy(),&home,ctx.is_project_trusted());
    let terminal=load_terminal_settings(settings.get_global().get("terminal"),settings.get_project().get("terminal"));
    let shell=settings.get().get("shellPath").and_then(Value::as_str).map(str::to_owned);
    Ok((terminal,shell))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_block_fills_defaults() {assert_eq!(resolve_terminal_settings(None),TERMINAL_SETTINGS_DEFAULTS);}

    #[test]
    fn valid_values_override_invalid_values_default() {
        let result=resolve_terminal_settings(Some(&json!({"defaultCols":200,"maxSessions":0,"notify":"off","timeoutAction":"bogus"})));
        assert_eq!(result.default_cols,200.0);assert_eq!(result.max_sessions,32.0);assert_eq!(result.notify,NotifyMode::Off);assert_eq!(result.timeout_action,TimeoutAction::Background);
    }

    #[test]
    fn monitor_delivery_settings_are_bounded() {
        let result=resolve_terminal_settings(Some(&json!({"monitorCoalesceWindowMs":1200,"monitorRateLimitMs":7000,"monitorMaxLinesPerInjection":75,"monitorMaxCharsPerInjection":8000,"monitorWakeBudget":9})));
        assert_eq!(result.monitor,MonitorDeliverySettings {coalesce_window_ms:1200.0,rate_limit_ms:7000.0,max_lines_per_injection:75.0,max_chars_per_injection:8000.0,wake_budget:9.0});
        assert_eq!(resolve_terminal_settings(Some(&json!({"monitorMaxLinesPerInjection":0}))).monitor.max_lines_per_injection,50.0);
        assert_eq!(resolve_terminal_settings(Some(&json!({"monitorWakeBudget":101}))).monitor.wake_budget,100.0);
    }

    #[test]
    fn project_layer_overrides_global_and_fractions_truncate() {
        let result=load_terminal_settings(Some(&json!({"defaultCols":200,"scrollback":0})),Some(&json!({"defaultCols":123.9,"notify":"next-turn"})));
        assert_eq!(result.default_cols,123.0);assert_eq!(result.scrollback,0.0);assert_eq!(result.notify,NotifyMode::NextTurn);
    }
}
