pub const DEFAULT_FOREGROUND_WINDOW_SECONDS:f64=60.0;
pub const SLEEP_WAIT_WINDOW_SECONDS:f64=5.0;

pub fn resolve_foreground_window_seconds(value:Option<&str>)->f64 {
    let parsed=value.and_then(|value| {
        let regex=regex::Regex::new(r"^[\s]*([+-]?(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?)").ok()?;
        regex.captures(value)?.get(1)?.as_str().parse::<f64>().ok()
    });
    parsed.filter(|value|value.is_finite() && *value>0.0).unwrap_or(DEFAULT_FOREGROUND_WINDOW_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn invalid_windows_default() {for value in [None,Some(""),Some("0"),Some("-1"),Some("Infinity"),Some("NaN")] {assert_eq!(resolve_foreground_window_seconds(value),60.0);}}
    #[test] fn js_float_prefixes_are_preserved() {assert_eq!(resolve_foreground_window_seconds(Some(" 2.5s")),2.5);assert_eq!(resolve_foreground_window_seconds(Some("1e2")),100.0);}
}
