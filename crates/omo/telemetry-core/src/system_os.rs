use std::process::Command;

use crate::types::TelemetryCpuInfo;
use crate::types::TelemetryError;
use crate::types::TelemetryOsProvider;

/// Reads host facts with the value vocabulary of `node:os` (`darwin`/`arm64`/`Darwin`, ...).
pub struct SystemTelemetryOsProvider;

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

impl TelemetryOsProvider for SystemTelemetryOsProvider {
    fn arch(&self) -> String {
        match std::env::consts::ARCH {
            "aarch64" => "arm64".to_string(),
            "x86_64" => "x64".to_string(),
            "x86" => "ia32".to_string(),
            "powerpc64" => "ppc64".to_string(),
            other => other.to_string(),
        }
    }

    fn cpus(&self) -> Result<Vec<TelemetryCpuInfo>, TelemetryError> {
        let parallelism = std::thread::available_parallelism()
            .map_err(|error| TelemetryError::new(error.to_string()))?
            .get();
        let model = cpu_model().unwrap_or_default();
        Ok(vec![TelemetryCpuInfo { model }; parallelism])
    }

    fn hostname(&self) -> String {
        gethostname::gethostname().to_string_lossy().into_owned()
    }

    fn platform(&self) -> String {
        match std::env::consts::OS {
            "macos" => "darwin".to_string(),
            "windows" => "win32".to_string(),
            other => other.to_string(),
        }
    }

    fn release(&self) -> String {
        command_output("uname", &["-r"]).unwrap_or_default()
    }

    fn totalmem(&self) -> u64 {
        total_memory_bytes().unwrap_or(0)
    }

    fn os_type(&self) -> String {
        if cfg!(windows) {
            return "Windows_NT".to_string();
        }
        command_output("uname", &["-s"]).unwrap_or_default()
    }
}

fn cpu_model() -> Option<String> {
    if cfg!(target_os = "macos") {
        return command_output("sysctl", &["-n", "machdep.cpu.brand_string"]);
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, model)| model.trim().to_string())
}

fn total_memory_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        return command_output("sysctl", &["-n", "hw.memsize"])?
            .parse()
            .ok();
    }
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}
