#[path = "schedule_delivery.rs"]
mod schedule_delivery;
#[path = "schedule_runner.rs"]
mod schedule_runner;
#[path = "schedule_watch.rs"]
mod schedule_watch;

use maho_core::schedule::{
    Result, occurrences::cancel_scheduled_job, runner_lease::*, store::*, types::iso_time,
};
use schedule_watch::{NativeProbes, RunOptions, run_passes, write_line};
use std::{io::Write, path::Path};

fn usage() -> String {
    format!(
        "usage: {} schedule <list|cancel|run> [options]\n\n  list    [--json]\n  cancel  <id>\n  run     [--watch] [--exec <command>]\n          [--poll-seconds <n>]     rescan interval (default 60)\n          [--timeout-seconds <n>]  delivery timeout (default 900)\n          [--concurrency <n>]      parallel sessions (default 4)\n",
        maho_core::config::app_command()
    )
}
fn positive_integer(flag: &str, value: Option<&String>) -> std::result::Result<u64, String> {
    let parsed = value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991);
    parsed.ok_or_else(|| format!("{flag} needs a positive integer"))
}
fn parse_run_options(args: &[String]) -> std::result::Result<RunOptions, String> {
    let mut options = RunOptions {
        watch: false,
        exec: None,
        poll_seconds: 60,
        timeout_seconds: 900,
        concurrency: 4,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--watch" => options.watch = true,
            "--exec" => {
                options.exec = Some(
                    args.next()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or("--exec needs a command")?
                        .clone(),
                )
            }
            "--poll-seconds" => options.poll_seconds = positive_integer(arg, args.next())?,
            "--timeout-seconds" => options.timeout_seconds = positive_integer(arg, args.next())?,
            "--concurrency" => {
                options.concurrency = usize::try_from(positive_integer(arg, args.next())?)
                    .map_err(|error| error.to_string())?
            }
            _ => return Err(format!("unknown option for run: {arg}")),
        }
    }
    Ok(options)
}
fn relative(ms: i128) -> String {
    let minutes = (ms.unsigned_abs() + 30_000) / 60_000;
    let text = if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 2880 {
        format!("{}h", (minutes + 30) / 60)
    } else {
        format!("{}d", (minutes + 720) / 1440)
    };
    if ms >= 0 {
        format!("in {text}")
    } else {
        format!("{text} ago")
    }
}
async fn list(dir: &Path, json: bool, probes: &dyn ProcessProbes) -> Result<i32> {
    let listing = list_scheduled_jobs(dir)?;
    let now = probes.now();
    let runners = live_runners(dir, probes).await?;
    let runner_values: Vec<_> = runners
        .iter()
        .map(|runner| {
            let mut value = serde_json::json!(runner);
            value["fresh"] =
                (now.saturating_sub(runner.beat_at) <= RUNNER_HEARTBEAT_STALE_MS).into();
            value
        })
        .collect();
    if json {
        let mut jobs = Vec::new();
        for record in &listing.jobs {
            let mut value = serde_json::to_value(&record.job)?;
            value["state"] = serde_json::to_value(record.state)?;
            value["file"] = record.file.clone().into();
            if let Some(occurrence) = record.occurrence {
                value["occurrence"] = occurrence.into();
            }
            jobs.push(value);
        }
        write_line(
            &serde_json::json!({"jobs":jobs,"invalid":listing.invalid,"runners":runner_values}),
        )?;
        return Ok(0);
    }
    let mut lines = Vec::new();
    for record in &listing.jobs {
        let job = &record.job;
        let every = job.every_ms.map_or_else(String::new, |every| {
            format!(" every {}s", (every + 500) / 1000)
        });
        let which = record
            .occurrence
            .map_or_else(String::new, |number| format!(" #{number}"));
        let error = job
            .last_error
            .as_ref()
            .map_or_else(String::new, |error| format!(" (error: {error})"));
        lines.push(format!(
            "{}{which}  {}  {} ({}){every}  session {}{error}\n    {}",
            job.id,
            record.state.directory(),
            iso_time(job.due_at)?,
            relative(i128::from(job.due_at) - i128::from(now)),
            job.session_id,
            job.prompt.split('\n').next().unwrap_or("")
        ));
    }
    for invalid in listing.invalid {
        lines.push(format!("{}  invalid: {}", invalid.file, invalid.error));
    }
    if lines.is_empty() {
        lines.push("No scheduled prompts.".into());
    }
    let watchers: Vec<_> = runners
        .iter()
        .filter(|runner| {
            runner.watch && now.saturating_sub(runner.beat_at) <= RUNNER_HEARTBEAT_STALE_MS
        })
        .map(|runner| runner.pid.to_string())
        .collect();
    lines.push(if watchers.is_empty() {
        format!(
            "Runner: none (start one with `{} schedule run --watch`)",
            maho_core::config::app_command()
        )
    } else {
        format!("Runner: active (pid {})", watchers.join(", "))
    });
    writeln!(std::io::stdout().lock(), "{}", lines.join("\n"))?;
    Ok(0)
}
enum Command {
    List(bool),
    Cancel(String),
    Run(RunOptions),
    Help,
}
fn parse(args: &[String]) -> std::result::Result<Command, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err("missing subcommand".into());
    };
    match command.as_str() {
        "list" => {
            if rest.iter().any(|arg| arg != "--json") {
                return Err(format!("unknown option for list: {}", rest.join(" ")));
            }
            Ok(Command::List(rest.iter().any(|arg| arg == "--json")))
        }
        "cancel" => {
            if rest.len() > 1 {
                return Err("cancel takes exactly one job id".into());
            }
            Ok(Command::Cancel(
                rest.first().ok_or("cancel needs a job id")?.clone(),
            ))
        }
        "run" => Ok(Command::Run(parse_run_options(rest)?)),
        "--help" | "-h" | "help" => Ok(Command::Help),
        _ => Err(format!("unknown subcommand: {command}")),
    }
}
pub async fn run_schedule_command(args: &[String]) -> std::result::Result<i32, String> {
    let command = match parse(args) {
        Ok(command) => command,
        Err(message) => {
            writeln!(std::io::stderr().lock(), "{message}\n{}", usage())
                .map_err(|error| error.to_string())?;
            return Ok(2);
        }
    };
    if matches!(command, Command::Help) {
        writeln!(std::io::stdout().lock(), "{}", usage()).map_err(|error| error.to_string())?;
        return Ok(0);
    }
    let dir = schedule_dir(Path::new(&maho_core::config::get_agent_dir()));
    let result: Result<i32> = async {
        match command {
            Command::List(json) => list(&dir, json, &NativeProbes::new().await?).await,
            Command::Cancel(id) => {
                let Some(result) = cancel_scheduled_job(&dir, &id, None)? else {
                    writeln!(std::io::stderr().lock(), "No scheduled prompt {id}.")?;
                    return Ok(1);
                };
                if result.in_flight { writeln!(std::io::stdout().lock(), "Cancelled {id}. One delivery had already started and may still complete; nothing after it will run.")?; }
                else { writeln!(std::io::stdout().lock(), "Cancelled {id}.")?; }
                Ok(0)
            },
            Command::Run(options) => run_passes(&dir, options, &NativeProbes::new().await?).await,
            Command::Help => Ok(0),
        }
    }.await;
    result.map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
