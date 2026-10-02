use maho_cli::utils::duration::format_duration;
macro_rules! case { ($name:ident, $milliseconds:expr, $expected:expr) => { #[test] fn $name() { assert_eq!(format_duration($milliseconds as f64), $expected); } }; }
case!(zero, 0, "0ms");
case!(milliseconds, 999, "999ms");
case!(second, 1000, "1.0s");
case!(fractional_seconds, 1500, "1.5s");
case!(rounded_seconds, 59999, "60.0s");
case!(minute, 60000, "1m 0s");
case!(minute_seconds, 90000, "1m 30s");
case!(last_minute, 3599999, "59m 59s");
case!(hour, 3600000, "1h 0m");
case!(last_hour, 86399999, "23h 59m");
case!(day, 86400000, "1d 0h");
case!(negative, -500, "-500ms");
