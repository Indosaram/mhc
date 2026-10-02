use std::{collections::BTreeMap, path::Path};

#[derive(Debug, PartialEq)]
pub struct SupervisorChildExit { pub code: Option<f64>, pub signal: Option<String> }
#[derive(Debug, PartialEq, Eq)]
pub enum SupervisorRuntimePlatform { Posix, Win32 }

pub fn parse_supervisor_child_exit(text: &str) -> Option<SupervisorChildExit> {
    let value: serde_json::Value = serde_json::from_str(text.trim().lines().last()?).ok()?;
    let code = value.get("code")?;
    let signal = value.get("signal")?;
    if (!code.is_number() && !code.is_null()) || (!signal.is_string() && !signal.is_null()) || signal == "MODEL_PID" { return None; }
    Some(SupervisorChildExit { code:code.as_f64(), signal:signal.as_str().map(str::to_owned) })
}

pub fn get_supervisor_runtime_platform(env: &BTreeMap<String, String>, platform: &str) -> SupervisorRuntimePlatform {
    if env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").is_some_and(|value| value == "1") {
        match env.get("OMO_MEMORY_SUPERVISOR_PLATFORM").map(String::as_str) { Some("posix") => return SupervisorRuntimePlatform::Posix, Some("win32") => return SupervisorRuntimePlatform::Win32, _ => {} }
    }
    if platform == "win32" { SupervisorRuntimePlatform::Win32 } else { SupervisorRuntimePlatform::Posix }
}

pub fn read_injected_clock(directory: &Path) -> f64 {
    let Ok(entries) = std::fs::read_dir(directory) else { return f64::NAN; };
    entries.flatten().filter_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (sequence, time) = name.split_once('-')?;
        if sequence.is_empty() || !sequence.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
        let digits = time.strip_prefix('-').unwrap_or(time);
        let mut parts = digits.split('.');
        let integer = parts.next()?;
        if integer.is_empty() || !integer.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
        if let Some(fraction) = parts.next() && (fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit())) { return None; }
        if parts.next().is_some() { return None; }
        Some((sequence.parse::<f64>().ok()?, time.parse::<f64>().ok()?))
    }).max_by(|left, right| left.0.total_cmp(&right.0)).map_or(f64::NAN, |(_, time)| time)
}

pub fn read_supervisor_clock_now(env: &BTreeMap<String, String>, now: impl FnOnce() -> f64) -> f64 {
    if env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").is_some_and(|value| value == "1") && let Some(path) = env.get("OMO_MEMORY_SUPERVISOR_CLOCK_PATH") { return read_injected_clock(Path::new(path)); }
    now()
}

pub fn get_supervisor_process_start(pid: u32) -> Option<String> { memory_core::locks::get_process_start_identity(pid) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exit_parser_uses_last_complete_line_and_rejects_pid_marker() { assert_eq!(parse_supervisor_child_exit("noise\n{\"code\":0,\"signal\":null}\n"),Some(SupervisorChildExit { code:Some(0.0), signal:None })); for invalid in ["","{\"code\":42,\"signal\":\"MODEL_PID\"}","{\"code\":\"0\",\"signal\":null}","{\"code\":0}"] { assert!(parse_supervisor_child_exit(invalid).is_none()); } }
    #[test] fn platform_override_is_test_seam_gated() { let mut env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_PLATFORM".into(),"win32".into())]); assert_eq!(get_supervisor_runtime_platform(&env,"linux"),SupervisorRuntimePlatform::Posix); env.insert("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS".into(),"1".into()); assert_eq!(get_supervisor_runtime_platform(&env,"linux"),SupervisorRuntimePlatform::Win32); }
    #[test] fn clock_uses_highest_numeric_sequence_not_time_or_filename_order() { let root=tempfile::tempdir().unwrap(); for name in ["2-200","10--4.5","junk","11-invalid","9-1000"] { std::fs::write(root.path().join(name),"").unwrap(); } assert_eq!(read_injected_clock(root.path()),-4.5); let env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_CLOCK_PATH".into(),root.path().to_string_lossy().into_owned())]); assert_eq!(read_supervisor_clock_now(&env,||12.0),12.0); }
    #[test] fn real_process_identity_matches_core() { assert_eq!(get_supervisor_process_start(std::process::id()),memory_core::locks::get_process_start_identity(std::process::id())); }
}
