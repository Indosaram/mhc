use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};

use super::exec::{ProcessKiller, create_default_process_killer};
use crate::runtime::node_platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSweepAction {
    Failed,
    Swept,
    Throttled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSweepStage {
    Kill,
    Terminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSweepFailure {
    pub error: String,
    pub pid: u32,
    pub stage: ProcessSweepStage,
}

/// Anything a sweep can signal.
pub trait SweepTarget {
    fn pid(&self) -> u32;
}

impl SweepTarget for super::process_table::ProcessInfo {
    fn pid(&self) -> u32 {
        self.pid
    }
}

#[derive(Default)]
pub struct ProcessFamilySweepOptions<'a> {
    pub dry_run: bool,
    pub force: bool,
    pub grace_ms: Option<u64>,
    pub killer: Option<&'a dyn ProcessKiller>,
    pub log: Option<&'a dyn Fn(&str)>,
    pub now_ms: Option<i64>,
    pub platform: Option<&'a str>,
    pub throttle_ms: Option<i64>,
}

impl ProcessFamilySweepOptions<'_> {
    pub(crate) fn log(&self, message: &str) {
        if let Some(log) = self.log {
            log(message);
        }
    }

    pub(crate) fn platform(&self) -> &str {
        self.platform.unwrap_or(node_platform())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFamilySweepResult<T, S = T> {
    pub action: ProcessSweepAction,
    pub candidates: Vec<T>,
    pub dry_run: bool,
    pub failed: Vec<ProcessSweepFailure>,
    pub killed: Vec<T>,
    pub spared: Vec<S>,
    pub stamp_file: PathBuf,
}

pub struct FamilySweepPlan<T, S = T> {
    pub candidates: Vec<T>,
    pub kill_list: Vec<T>,
    pub spared: Vec<S>,
}

/// Re-proves target identity immediately before each signal; `Err` spares the target.
pub type SignalAttestation<'a, T> = &'a dyn Fn(&T) -> Result<bool, String>;

pub struct FamilySweepConfig<'a, T, S = T> {
    pub attest_before_signal: Option<SignalAttestation<'a, T>>,
    pub family_label: &'a str,
    pub stamp_file: PathBuf,
    pub collect: &'a dyn Fn() -> Result<FamilySweepPlan<T, S>, String>,
}

const DEFAULT_GRACE_MS: u64 = 2_000;
const DEFAULT_THROTTLE_MS: i64 = 60 * 60 * 1_000;

pub fn run_process_family_sweep<T: SweepTarget + Clone, S>(
    config: &FamilySweepConfig<'_, T, S>,
    options: &ProcessFamilySweepOptions<'_>,
) -> ProcessFamilySweepResult<T, S> {
    let now_ms = options.now_ms.unwrap_or_else(current_ms);
    let dry_run = options.dry_run;
    let empty = |action| ProcessFamilySweepResult {
        action,
        candidates: Vec::new(),
        dry_run,
        failed: Vec::new(),
        killed: Vec::new(),
        spared: Vec::new(),
        stamp_file: config.stamp_file.clone(),
    };

    if !options.force
        && is_sweep_throttled(
            &config.stamp_file,
            now_ms,
            options.throttle_ms.unwrap_or(DEFAULT_THROTTLE_MS),
        )
    {
        return empty(ProcessSweepAction::Throttled);
    }

    let outcome = (config.collect)().and_then(|plan| {
        let (failed, killed) = if dry_run {
            (Vec::new(), Vec::new())
        } else {
            kill_targets(&plan.kill_list, options, config)
        };
        if !dry_run {
            write_sweep_stamp(&config.stamp_file, now_ms)?;
        }
        Ok(ProcessFamilySweepResult {
            action: ProcessSweepAction::Swept,
            candidates: plan.candidates,
            dry_run,
            failed,
            killed,
            spared: plan.spared,
            stamp_file: config.stamp_file.clone(),
        })
    });
    match outcome {
        Ok(result) => result,
        Err(error) => {
            options.log(&format!("{} skipped: {error}", config.family_label));
            empty(ProcessSweepAction::Failed)
        }
    }
}

fn kill_targets<T: SweepTarget + Clone, S>(
    targets: &[T],
    options: &ProcessFamilySweepOptions<'_>,
    config: &FamilySweepConfig<'_, T, S>,
) -> (Vec<ProcessSweepFailure>, Vec<T>) {
    let default_killer;
    let killer: &dyn ProcessKiller = match options.killer {
        Some(killer) => killer,
        None => {
            default_killer = create_default_process_killer(options.platform());
            default_killer.as_ref()
        }
    };
    let mut failed = Vec::new();
    let mut killed = Vec::new();
    for target in targets {
        let pid = target.pid();
        if !passes_signal_attestation(target, config.attest_before_signal, options, "SIGTERM") {
            continue;
        }
        if let Err(error) = killer.terminate(pid) {
            options.log(&format!(
                "{} failed to terminate pid {pid}: {error}",
                config.family_label
            ));
            failed.push(ProcessSweepFailure {
                error,
                pid,
                stage: ProcessSweepStage::Terminate,
            });
            continue;
        }
        let grace_ms = options.grace_ms.unwrap_or(DEFAULT_GRACE_MS);
        if grace_ms > 0 {
            std::thread::sleep(Duration::from_millis(grace_ms));
        }
        if !killer.is_alive(pid) {
            killed.push(target.clone());
            continue;
        }
        if !passes_signal_attestation(target, config.attest_before_signal, options, "SIGKILL") {
            continue;
        }
        match killer.kill(pid) {
            Ok(()) => killed.push(target.clone()),
            Err(error) => {
                options.log(&format!(
                    "{} failed to kill pid {pid}: {error}",
                    config.family_label
                ));
                failed.push(ProcessSweepFailure {
                    error,
                    pid,
                    stage: ProcessSweepStage::Kill,
                });
            }
        }
    }
    (failed, killed)
}

fn passes_signal_attestation<T: SweepTarget>(
    target: &T,
    attest: Option<SignalAttestation<'_, T>>,
    options: &ProcessFamilySweepOptions<'_>,
    signal: &str,
) -> bool {
    let Some(attest) = attest else {
        return true;
    };
    let pid = target.pid();
    match attest(target) {
        Ok(true) => true,
        Ok(false) => {
            options.log(&format!(
                "process sweep sparing pid {pid}: identity changed before {signal}"
            ));
            false
        }
        Err(error) => {
            options.log(&format!("process sweep sparing pid {pid}: identity attestation failed before {signal}: {error}"));
            false
        }
    }
}

fn is_sweep_throttled(stamp_file: &Path, now_ms: i64, throttle_ms: i64) -> bool {
    let Ok(modified) = fs::metadata(stamp_file).and_then(|meta| meta.modified()) else {
        return false;
    };
    now_ms - system_time_ms(modified) < throttle_ms
}

fn write_sweep_stamp(stamp_file: &Path, now_ms: i64) -> Result<(), String> {
    if let Some(parent) = stamp_file.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let stamp = DateTime::<Utc>::from_timestamp_millis(now_ms).map_or_else(
        || now_ms.to_string(),
        |date| date.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    fs::write(stamp_file, format!("{stamp}\n")).map_err(|error| error.to_string())?;
    let mtime = ms_to_system_time(now_ms);
    fs::File::options()
        .write(true)
        .open(stamp_file)
        .and_then(|file| {
            file.set_times(fs::FileTimes::new().set_accessed(mtime).set_modified(mtime))
        })
        .map_err(|error| error.to_string())
}

pub(crate) fn current_ms() -> i64 {
    system_time_ms(SystemTime::now())
}

fn system_time_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

fn ms_to_system_time(ms: i64) -> SystemTime {
    let magnitude = Duration::from_millis(ms.unsigned_abs());
    if ms >= 0 {
        UNIX_EPOCH + magnitude
    } else {
        UNIX_EPOCH - magnitude
    }
}
