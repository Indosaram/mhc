//! Resolve the bound project's sticky space before opening a trust-pinned account session.
pub type EndedCallback = Rc<RefCell<Vec<Box<dyn Fn()>>>>;
use super::account_source::{DeviceAccount, DeviceAccountSource};
use memory_core::{
    identity::layout::MemoryIdentityPaths,
    sync::{
        engine::SyncSession,
        keyring_port::DeviceKeyMaterial,
        space::{GitSpaceConfig, ResolveSpaceOptions, list_remotes, resolve_space},
    },
};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

#[derive(Clone, Debug)]
pub struct SyncSessionSpace {
    pub profile: Option<String>,
    pub project_dir: PathBuf,
}
#[derive(Clone, Debug)]
pub struct SyncSessionRequest {
    pub identity: String,
    pub paths: MemoryIdentityPaths,
    pub space: SyncSessionSpace,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountSessionIdle {
    NoPin,
    CorruptPin,
    NotMember,
}
pub enum SyncSessionResolution {
    SignedOut,
    Idle(AccountSessionIdle),
    Open(ResolvedDeviceSession),
}
pub struct ResolvedDeviceSession {
    pub device_name: Option<crate::sync_runner::DeviceNameLookup>,
    pub session: SyncSession,
    pub ended: EndedCallback,
}
impl ResolvedDeviceSession {
    pub fn on_ended(&self, notify: Box<dyn Fn()>) {
        self.ended.borrow_mut().push(notify);
    }
}
pub type LocalFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>>>>;
pub struct AccountSessionOptions {
    pub api: String,
    pub account_id: String,
    pub device_id: String,
    pub material: DeviceKeyMaterial,
    pub pin_dir: PathBuf,
    pub space_id: [u8; 16],
    pub token: Rc<dyn Fn() -> LocalFuture<String>>,
}
pub type OpenAccountSession = Arc<
    dyn Fn(AccountSessionOptions) -> LocalFuture<Result<SyncSession, AccountSessionIdle>>
        + Send
        + Sync,
>;
pub type DeviceSessionResolver =
    Arc<dyn Fn(SyncSessionRequest) -> LocalFuture<SyncSessionResolution> + Send + Sync>;
pub fn create_device_session_resolver(
    source: DeviceAccountSource,
    pin_dir: PathBuf,
    open: OpenAccountSession,
) -> DeviceSessionResolver {
    Arc::new(move |request| {
        let source = source.clone();
        let pin_dir = pin_dir.clone();
        let open = open.clone();
        Box::pin(async move {
            let Some(account) = source().await? else {
                return Ok(SyncSessionResolution::SignedOut);
            };
            let remotes =
                list_remotes(&request.space.project_dir).map_err(|error| error.to_string())?;
            let resolved = resolve_space(
                &mut GitSpaceConfig {
                    repo_dir: request.paths.repo,
                },
                ResolveSpaceOptions {
                    profile: request.space.profile.as_deref(),
                    remotes: &remotes,
                    random_hex: None,
                },
            )
            .map_err(|error| error.to_string())?;
            let mut space_id = [0; 16];
            if resolved.space_id.len() != 32 {
                return Err("memory space id must be 16 bytes".into());
            }
            for (index, byte) in space_id.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&resolved.space_id[index * 2..index * 2 + 2], 16)
                    .map_err(|error| error.to_string())?;
            }
            let ended = Rc::new(RefCell::new(Vec::<Box<dyn Fn()>>::new()));
            let token = account_token(account.clone(), ended.clone());
            let options = AccountSessionOptions {
                api: account.api.clone(),
                account_id: account.account_id.clone(),
                device_id: account.device_id.clone(),
                material: DeviceKeyMaterial {
                    signing_private_pkcs8: account.material.signing_private_pkcs8.clone(),
                    sealing_private_pkcs8: account.material.sealing_private_pkcs8.clone(),
                },
                pin_dir,
                space_id,
                token,
            };
            Ok(match open(options).await? {
                Ok(session) => SyncSessionResolution::Open(ResolvedDeviceSession {
                    device_name: None,
                    session,
                    ended,
                }),
                Err(idle) => SyncSessionResolution::Idle(idle),
            })
        })
    })
}
fn account_token(
    account: Arc<DeviceAccount>,
    ended: EndedCallback,
) -> Rc<dyn Fn() -> LocalFuture<String>> {
    Rc::new(move || {
        let account = account.clone();
        let ended = ended.clone();
        Box::pin(async move {
            match account.access_token().await {
                Ok(token) => Ok(token),
                Err(error) => {
                    if account.signed_out().await? {
                        for notify in ended.borrow().iter() {
                            notify();
                        }
                    }
                    Err(error)
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn signed_out_never_opens_or_resolves_space() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let source: DeviceAccountSource = Arc::new(|| Box::pin(async { Ok(None) }));
        let resolver = create_device_session_resolver(
            source,
            root.path().join("pins"),
            Arc::new(|_| panic!("signed-out account must not open")),
        );
        let result = resolver(SyncSessionRequest {
            identity: "agent".into(),
            paths: paths.clone(),
            space: SyncSessionSpace {
                profile: None,
                project_dir: root.path().into(),
            },
        })
        .await
        .unwrap();
        assert!(matches!(result, SyncSessionResolution::SignedOut));
        assert!(!paths.repo.exists());
    }
}
