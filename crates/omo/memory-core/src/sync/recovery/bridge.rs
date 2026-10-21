use super::{
    bridge_enroll::enroll_in_bridge,
    bridge_recover::{recover_begin_in_bridge, recover_finish_in_bridge},
    ports::*,
    recovery_handles::RecoveryHandles,
};
use crate::sync::keyring_port::DeviceKeyring;
use std::sync::Arc;
pub struct ReferenceBridge<K> {
    authenticator: Arc<dyn PasskeyAuthenticator>,
    keyring: K,
    handles: RecoveryHandles,
}
pub fn create_recovery_bridge<K: DeviceKeyring + Send + Sync>(
    authenticator: Arc<dyn PasskeyAuthenticator>,
    keyring: K,
) -> ReferenceBridge<K> {
    ReferenceBridge {
        authenticator,
        keyring,
        handles: RecoveryHandles::default(),
    }
}
impl<K: DeviceKeyring + Send + Sync> RecoveryBridge for ReferenceBridge<K> {
    fn prf_supported(&self) -> bool {
        self.authenticator.prf_supported()
    }
    fn enroll(&self, request: EnrollBridgeRequest) -> PortFuture<'_, EnrollBridgeResult> {
        Box::pin(enroll_in_bridge(
            &*self.authenticator,
            &self.keyring,
            request,
        ))
    }
    fn recover_begin(&self, request: RecoverBeginRequest) -> PortFuture<'_, RecoverBeginResult> {
        Box::pin(recover_begin_in_bridge(
            &*self.authenticator,
            &self.handles,
            request,
        ))
    }
    fn recover_finish(&self, request: RecoverFinishRequest) -> PortFuture<'_, RecoverBridgeResult> {
        Box::pin(async move { recover_finish_in_bridge(&self.handles, request) })
    }
    fn recover_abort(&self, handle: &str) {
        drop(self.handles.take(handle));
    }
    fn signal_removed<'a>(
        &'a self,
        rp_id: &'a str,
        credential_id: &'a str,
    ) -> PortFuture<'a, PlatformSignal> {
        self.authenticator.signal_removed(rp_id, credential_id)
    }
}
