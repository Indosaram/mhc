use crate::owner::{OwnerProbe, OwnerStatus};

pub fn get_process_start_identity(pid: u32) -> Option<String> {
    memory_core::locks::get_process_start_identity(pid)
}

fn identity_scheme(identity: &str) -> Option<&str> {
    match identity.find(':') {
        Some(0) | None => None,
        Some(index) => Some(&identity[..index]),
    }
}

pub fn start_identities_conflict(recorded: &str, actual: &str) -> bool {
    let Some(recorded_scheme) = identity_scheme(recorded) else {
        return false;
    };
    if Some(recorded_scheme) != identity_scheme(actual) {
        return false;
    }
    recorded != actual
}

#[derive(Debug, Clone, Copy)]
enum SignalZero {
    Alive,
    NoSuchProcess,
    Unknown,
}

#[cfg(unix)]
fn signal_zero(pid: u32) -> SignalZero {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return SignalZero::Alive;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::ESRCH => SignalZero::NoSuchProcess,
        _ => SignalZero::Unknown,
    }
}

#[cfg(not(unix))]
fn signal_zero(pid: u32) -> SignalZero {
    match memory_core::locks::get_pid_liveness(pid) {
        memory_core::locks::ProcessLiveness::Alive => SignalZero::Alive,
        memory_core::locks::ProcessLiveness::Dead => SignalZero::NoSuchProcess,
        memory_core::locks::ProcessLiveness::Unknown => SignalZero::Unknown,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessOwnerProbe;

impl OwnerProbe for ProcessOwnerProbe {
    fn pid_alive(&self, pid: u32, start_identity: Option<&str>) -> OwnerStatus {
        match signal_zero(pid) {
            SignalZero::Alive => {}
            SignalZero::NoSuchProcess => return OwnerStatus::Dead,
            SignalZero::Unknown => return OwnerStatus::Unknown,
        }
        let current = get_process_start_identity(pid);
        match (start_identity, current) {
            (Some(recorded), Some(actual)) if start_identities_conflict(recorded, &actual) => {
                OwnerStatus::Dead
            }
            _ => OwnerStatus::Alive,
        }
    }
}

pub fn process_owner_probe() -> ProcessOwnerProbe {
    ProcessOwnerProbe
}
