//! `mhc usage [--json] [--sessions] [--days N] [--file PATH] [--status]`
//! (docs/cleanroom/specs/telemetry.md "`mhc usage`"; the report itself is the oh-my-pi
//! `omp stats --summary`/`--json` port in [`crate::stats`]).
use crate::record::{Ledger, now_ms, scan_sessions};
use crate::settings::Settings;
use crate::stats::{report, summary_text};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "Usage: mhc usage [options]

Model usage, tokens and estimated cost.

Options:
  --json          Print the report as JSON
  --sessions      Read the session logs instead of the usage ledger
  --days <n>      Only the last <n> days
  --file <path>   Read another ledger file
  --status        Show the telemetry settings (ledger, OTLP export, opt-outs)

Ledger: $MAHO_USAGE_DIR/usage.jsonl, else <agent dir>/stats/usage.jsonl.
Opt out: MAHO_TELEMETRY=0 (everything), MAHO_USAGE_STATS=0 (ledger), DO_NOT_TRACK=1 or
OTEL_SDK_DISABLED=true (export), or the telemetry.* config keys.
";

#[derive(Debug, Default, PartialEq)]
struct Options {
    json: bool,
    sessions: bool,
    status: bool,
    help: bool,
    days: Option<u32>,
    file: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" | "-j" => options.json = true,
            "--sessions" => options.sessions = true,
            "--status" => options.status = true,
            "-h" | "--help" | "help" => options.help = true,
            "--days" => {
                let value = iter.next().ok_or("--days needs a number")?;
                options.days = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--days: not a number: {value}"))?,
                );
            }
            "--file" => {
                options.file = Some(PathBuf::from(iter.next().ok_or("--file needs a path")?));
            }
            other => return Err(format!("mhc usage: unknown option {other}\n\n{USAGE}")),
        }
    }
    Ok(options)
}

/// Runs `mhc usage`; returns the text to print on stdout.
pub fn run(args: &[String], settings: &Settings, sessions_dir: &Path) -> Result<String, String> {
    let options = parse(args)?;
    if options.help {
        return Ok(USAGE.trim_end().to_owned());
    }
    if options.status {
        return Ok(settings.status_text());
    }
    let (source, records) = if options.sessions {
        ("sessions".to_owned(), scan_sessions(sessions_dir))
    } else {
        let path = options
            .file
            .clone()
            .unwrap_or_else(|| settings.ledger_location.clone());
        let records = Ledger::new(&path)
            .read()
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        (path.display().to_string(), records)
    };
    let since = options
        .days
        .map(|days| now_ms() - i64::from(days) * 86_400_000);
    let report = report(&source, &records, since);
    if options.json {
        return serde_json::to_string_pretty(&report).map_err(|error| error.to_string());
    }
    let mut text = summary_text(&report);
    if !options.sessions && options.file.is_none() && settings.ledger.is_none() {
        text.push_str(&format!(
            "\n\nThe usage ledger is off ({}).",
            settings.ledger_off.join("; ")
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::UsageRecord;
    use crate::settings::{ConfigSwitches, resolve};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn settings(dir: &Path, pairs: &[(&str, &str)]) -> Settings {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        resolve(
            &|key| map.get(key).cloned(),
            ConfigSwitches::default(),
            dir,
            "0",
        )
    }

    #[test]
    fn reports_the_ledger_as_text_and_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(dir.path(), &[]);
        let ledger = Ledger::new(settings.ledger.clone().expect("on"));
        assert_eq!(
            run(&[], &settings, dir.path()).expect("run"),
            "No usage recorded yet."
        );
        let record = UsageRecord {
            timestamp: now_ms(),
            provider: "p".into(),
            model: "m".into(),
            stop_reason: "stop".into(),
            total_tokens: 3,
            output: 3,
            ..UsageRecord::default()
        };
        ledger.append(&record).expect("append");
        let text = run(&[], &settings, dir.path()).expect("run");
        assert!(text.contains("Requests: 1 (0 errors)"), "{text}");
        let json: serde_json::Value = serde_json::from_str(
            &run(&args(&["--json", "--days", "1"]), &settings, dir.path()).expect("run"),
        )
        .expect("json");
        assert_eq!(json["records"], 1);
        assert_eq!(json["byModel"][0]["model"], "m");
        assert!(run(&args(&["--bogus"]), &settings, dir.path()).is_err());
        assert!(run(&args(&["--days", "x"]), &settings, dir.path()).is_err());
        assert!(
            run(&args(&["--help"]), &settings, dir.path())
                .expect("help")
                .starts_with("Usage: mhc usage")
        );
    }

    #[test]
    fn status_and_ledger_off_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = settings(
            dir.path(),
            &[("MAHO_USAGE_STATS", "0"), ("DO_NOT_TRACK", "1")],
        );
        let status = run(&args(&["--status"]), &settings, dir.path()).expect("status");
        assert!(
            status.contains("Usage ledger: off (MAHO_USAGE_STATS is off)"),
            "{status}"
        );
        assert!(
            status.contains("OTLP export: off (DO_NOT_TRACK is set)"),
            "{status}"
        );
        let text = run(&[], &settings, dir.path()).expect("run");
        assert!(
            text.ends_with("The usage ledger is off (MAHO_USAGE_STATS is off)."),
            "{text}"
        );
        let sessions = run(&args(&["--sessions"]), &settings, dir.path()).expect("run");
        assert_eq!(sessions, "No usage recorded yet.");
    }
}
