//! Lazy sidecar wake wrappers (`thread/wake/wake-runtime.ts`).

use std::path::PathBuf;
use std::sync::Arc;

use super::super::intake_admission::IntakeAdmission;
use super::super::same_device_wake::SameDeviceWake;
use super::super::store_api::GatewayStoreApi;
use super::runtime::{
    RunningThreadWake, ThreadWakeOptions, process_intake_admission, process_same_device_wake,
    start_thread_wake_at_session,
};

pub fn start_wake_at_session(
    mode: &str,
    agent_dir: PathBuf,
    sessions_directory: PathBuf,
    options: ThreadWakeOptions,
) -> Option<Arc<RunningThreadWake>> {
    start_thread_wake_at_session(mode, agent_dir, sessions_directory, options)
}

pub fn create_same_device_wake(
    store: Arc<dyn GatewayStoreApi>,
    agent_dir: PathBuf,
    sessions_directory: PathBuf,
    enabled: Arc<dyn Fn(&str) -> bool + Send + Sync>,
) -> SameDeviceWake {
    process_same_device_wake(store, agent_dir, sessions_directory, enabled)
}

pub async fn intake_admission(agent_dir: &PathBuf, device: &str) -> IntakeAdmission {
    process_intake_admission(agent_dir, device).await
}
