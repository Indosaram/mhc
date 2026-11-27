//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-secret-native.ts`.
//!
//! Headless loader for the Windows socket-secret bridge (`crates/senpi-socket-secret`). There is no
//! optional fallback: a Windows public-secret mutation that cannot load the bridge must fail, never
//! fall back to a direct final-path write.

pub use crate::socket_secret_publish::{
    LeftBehindStaging, NativeLeftBehindStaging, NativePublishResult, SocketSecretOutcome,
    SocketSecretPublication, SocketSecretPublishBinding, SocketSecretPublishError,
    publish_socket_secret_no_replace,
};
use futures::future::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use maho_core::config::get_package_dir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const NATIVE_SOCKET_SECRET_ABI_VERSION: &str = "1";
pub const SOCKET_SECRET_NATIVE_PATH_ENV: &str = "SENPI_SOCKET_SECRET_NATIVE_PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeKernelSessionIdentity {
    pub status: KernelSessionStatus,
    pub creation_time: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelSessionStatus {
    Verified,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVolumeLocality {
    pub locality: VolumeLocalityKind,
    pub reason: Option<String>,
    pub reported_file_system: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeLocalityKind {
    Local,
    NotLocal,
}

pub trait NativeSocketSecretBinding: SocketSecretPublishBinding + Send + Sync {
    fn abi_version(&self) -> &str;
    fn kernel_session_identity(&self) -> NativeKernelSessionIdentity;
    fn classify_volume(&self, path: &str) -> NativeVolumeLocality;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePresenceResult {
    Acquired,
    Occupied,
    Valid,
    Released,
    Cancelled,
    Unknown { reason: String, code: Option<i64> },
    Fenced { reason: String, code: Option<i64> },
    UnavailableUnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePresenceProbe {
    pub presence_id: String,
    pub path: String,
    pub header: Vec<u8>,
    pub volume_serial: String,
    pub file_id: String,
    pub created_local: bool,
    pub relocated: bool,
    pub kernel_session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceReleaseReason {
    Released,
    RecoveryAbandoned,
    Discharged,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceReleasedReceipt {
    pub presence_id: String,
    pub state: PresenceReleaseReason,
}

pub trait NativePresenceBinding: Send + Sync {
    fn presence_acquire(&self, presence_id: &str, path: &str) -> NativePresenceResult;
    fn presence_probe(&self, request: &NativePresenceProbe) -> NativePresenceResult;
    fn presence_probe_retained(&self, request: &NativePresenceProbe) -> NativePresenceResult;
    fn presence_pre_action_check(&self, presence_id: &str) -> NativePresenceResult;
    fn presence_wait_and_acquire(
        &self,
        presence_id: String,
        path: String,
        wait_id: String,
    ) -> BoxFuture<'static, NativePresenceResult>;
    fn presence_cancel_wait(&self, wait_id: &str);
    fn presence_release_after_quiescence(
        &self,
        receipt: &PresenceReleasedReceipt,
    ) -> NativePresenceResult;
}

pub struct SocketSecretPresence {
    binding: Arc<dyn NativePresenceBinding>,
}

impl SocketSecretPresence {
    pub fn acquire(&self, presence_id: &str, path: &str) -> NativePresenceResult {
        self.binding.presence_acquire(presence_id, path)
    }

    pub fn probe(&self, request: &NativePresenceProbe) -> NativePresenceResult {
        self.binding.presence_probe(request)
    }

    pub fn probe_retained(&self, request: &NativePresenceProbe) -> NativePresenceResult {
        self.binding.presence_probe_retained(request)
    }

    pub fn pre_action_check(&self, presence_id: &str) -> NativePresenceResult {
        self.binding.presence_pre_action_check(presence_id)
    }

    pub async fn wait_and_acquire(
        &self,
        presence_id: &str,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> NativePresenceResult {
        if signal.is_some_and(AbortSignal::aborted) {
            return NativePresenceResult::Cancelled;
        }
        let wait_id = uuid::Uuid::new_v4().to_string();
        let waiting = self.binding.presence_wait_and_acquire(
            presence_id.to_owned(),
            path.to_owned(),
            wait_id.clone(),
        );
        match signal {
            None => waiting.await,
            Some(sig) => {
                tokio::pin!(waiting);
                tokio::select! {
                    biased;
                    () = sig.cancelled() => {
                        self.binding.presence_cancel_wait(&wait_id);
                        waiting.await
                    }
                    res = &mut waiting => res,
                }
            }
        }
    }

    pub fn release_after_quiescence(
        &self,
        receipt: &PresenceReleasedReceipt,
    ) -> NativePresenceResult {
        self.binding.presence_release_after_quiescence(receipt)
    }
}

pub enum PresenceLoadResult {
    Available(SocketSecretPresence),
    UnavailableUnsupportedPlatform,
}

#[derive(Default)]
pub struct SocketSecretNativeOptions {
    pub package_dir: Option<String>,
    pub module_dir: Option<String>,
    pub exec_path: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub platform: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The Windows socket-secret bridge for {host} could not be loaded: {causes_joined}")]
pub struct SocketSecretNativeUnavailableError {
    pub host: String,
    pub attempted_paths: Vec<String>,
    pub causes_joined: String,
}

impl SocketSecretNativeUnavailableError {
    pub const CODE: &'static str = "socket-secret-native-unavailable";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Socket-secret bridge ABI mismatch in {module_path}: expected __senpiSocketSecretAbi1() === \"{NATIVE_SOCKET_SECRET_ABI_VERSION}\" and every export"
)]
pub struct SocketSecretNativeAbiMismatchError {
    pub module_path: String,
}

impl SocketSecretNativeAbiMismatchError {
    pub const CODE: &'static str = "socket-secret-native-abi-mismatch";
}

#[derive(Debug, thiserror::Error)]
pub enum SocketSecretNativeLoadError {
    #[error(transparent)]
    Unavailable(#[from] SocketSecretNativeUnavailableError),
    #[error(transparent)]
    AbiMismatch(#[from] SocketSecretNativeAbiMismatchError),
}

fn current_node_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn current_node_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

pub fn socket_secret_native_candidate_paths(options: &SocketSecretNativeOptions) -> Vec<String> {
    let env_override = options
        .env
        .as_ref()
        .and_then(|e| e.get(SOCKET_SECRET_NATIVE_PATH_ENV).cloned())
        .or_else(|| std::env::var(SOCKET_SECRET_NATIVE_PATH_ENV).ok());
    if let Some(override_path) = env_override.filter(|s| !s.is_empty()) {
        return vec![override_path];
    }
    let platform = options
        .platform
        .as_deref()
        .unwrap_or(current_node_platform());
    let arch = options.arch.as_deref().unwrap_or(current_node_arch());
    let host = format!("{platform}-{arch}");
    let relative = Path::new("native")
        .join("socket-secret")
        .join("prebuilds")
        .join(&host)
        .join(format!("senpi_socket_secret.{host}.node"));
    let package_dir = options
        .package_dir
        .clone()
        .or_else(|| {
            options
                .env
                .as_ref()
                .and_then(|e| e.get("SENPI_PACKAGE_DIR").cloned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(get_package_dir);
    let module_dir = options
        .module_dir
        .clone()
        .unwrap_or_else(|| package_dir.clone());
    let exec_dir = options
        .exec_path
        .as_deref()
        .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    vec![
        PathBuf::from(&package_dir)
            .join(&relative)
            .display()
            .to_string(),
        PathBuf::from(&module_dir)
            .join("../../..")
            .join(&relative)
            .display()
            .to_string(),
        exec_dir.join(&relative).display().to_string(),
    ]
}

pub fn load_socket_secret_native<F>(
    options: &SocketSecretNativeOptions,
    mut require_binding: F,
) -> Result<Arc<dyn NativeSocketSecretBinding>, SocketSecretNativeLoadError>
where
    F: FnMut(&str) -> Result<Arc<dyn NativeSocketSecretBinding>, String>,
{
    let attempted_paths = socket_secret_native_candidate_paths(options);
    let mut causes = Vec::new();
    for module_path in &attempted_paths {
        match require_binding(module_path) {
            Ok(binding) => {
                if binding.abi_version() != NATIVE_SOCKET_SECRET_ABI_VERSION {
                    return Err(SocketSecretNativeLoadError::AbiMismatch(
                        SocketSecretNativeAbiMismatchError {
                            module_path: module_path.clone(),
                        },
                    ));
                }
                return Ok(binding);
            }
            Err(err) => causes.push(format!("{module_path}: {err}")),
        }
    }
    let host = format!(
        "{}-{}",
        options
            .platform
            .as_deref()
            .unwrap_or_else(|| current_node_platform()),
        options
            .arch
            .as_deref()
            .unwrap_or_else(|| current_node_arch()),
    );
    Err(SocketSecretNativeLoadError::Unavailable(
        SocketSecretNativeUnavailableError {
            host,
            attempted_paths,
            causes_joined: causes.join("; "),
        },
    ))
}

pub fn load_socket_secret_presence(
    options: &SocketSecretNativeOptions,
    native: Option<Arc<dyn NativePresenceBinding>>,
) -> PresenceLoadResult {
    let platform = options
        .platform
        .as_deref()
        .unwrap_or_else(|| current_node_platform());
    if platform != "win32" {
        return PresenceLoadResult::UnavailableUnsupportedPlatform;
    }
    match native {
        Some(binding) => PresenceLoadResult::Available(SocketSecretPresence { binding }),
        None => PresenceLoadResult::UnavailableUnsupportedPlatform,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_takes_precedence_and_non_win32_is_unsupported() {
        let opts = SocketSecretNativeOptions {
            env: Some(HashMap::from([(
                SOCKET_SECRET_NATIVE_PATH_ENV.to_owned(),
                "/custom/bridge.node".to_owned(),
            )])),
            platform: Some("linux".into()),
            ..Default::default()
        };
        assert_eq!(
            socket_secret_native_candidate_paths(&opts),
            vec!["/custom/bridge.node"]
        );
        assert!(matches!(
            load_socket_secret_presence(&opts, None),
            PresenceLoadResult::UnavailableUnsupportedPlatform
        ));
    }

    #[test]
    fn abi_mismatch_stops_immediately() {
        struct BadAbi;
        impl SocketSecretPublishBinding for BadAbi {
            fn publish_no_replace(
                &self,
                _: &str,
                _: &str,
                _: &[u8],
            ) -> Result<serde_json::Value, String> {
                Ok(serde_json::Value::Null)
            }
        }
        impl NativeSocketSecretBinding for BadAbi {
            fn abi_version(&self) -> &str {
                "99"
            }
            fn kernel_session_identity(&self) -> NativeKernelSessionIdentity {
                NativeKernelSessionIdentity {
                    status: KernelSessionStatus::Unverified,
                    creation_time: None,
                    reason: None,
                }
            }
            fn classify_volume(&self, _: &str) -> NativeVolumeLocality {
                NativeVolumeLocality {
                    locality: VolumeLocalityKind::Local,
                    reason: None,
                    reported_file_system: None,
                }
            }
        }
        let opts = SocketSecretNativeOptions {
            package_dir: Some("/pkg".into()),
            platform: Some("win32".into()),
            arch: Some("x64".into()),
            ..Default::default()
        };
        let mut calls = 0;
        let err = match load_socket_secret_native(&opts, |_| {
            calls += 1;
            Ok(Arc::new(BadAbi))
        }) {
            Ok(_) => panic!("expected abi mismatch error"),
            Err(err) => err,
        };
        assert_eq!(calls, 1);
        assert!(matches!(err, SocketSecretNativeLoadError::AbiMismatch(_)));
    }
}
