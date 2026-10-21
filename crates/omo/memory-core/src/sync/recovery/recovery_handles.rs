use super::{
    RecoveryError,
    derive::{RecoveryKeyPairs, random},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
pub const RECOVERY_HANDLE_TTL_MS: u64 = 120_000;
pub struct HeldRecovery {
    pub account: String,
    pub credential_id: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
    pub keys: RecoveryKeyPairs,
}
struct Live {
    handle: String,
    expires_at: Instant,
    recovery: HeldRecovery,
    timer: Option<tokio::task::AbortHandle>,
}
pub struct RecoveryHandles {
    live: Arc<Mutex<Option<Live>>>,
    #[cfg(test)]
    expired: Arc<tokio::sync::Notify>,
}
impl Default for RecoveryHandles {
    fn default() -> Self {
        Self {
            live: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            expired: Arc::new(tokio::sync::Notify::new()),
        }
    }
}
impl RecoveryHandles {
    pub fn hold(&self, recovery: HeldRecovery) -> Result<String, RecoveryError> {
        self.dispose_all();
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| RecoveryError::InvalidInput("recovery handles require a Tokio runtime"))?;
        let handle = URL_SAFE_NO_PAD.encode(random::<32>()?);
        let expires_at = Instant::now() + Duration::from_millis(RECOVERY_HANDLE_TTL_MS);
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *live = Some(Live {
            handle: handle.clone(),
            expires_at,
            recovery,
            timer: None,
        });
        let weak = Arc::downgrade(&self.live);
        let timer_handle = handle.clone();
        #[cfg(test)]
        let expired = Arc::clone(&self.expired);
        let timer = runtime.spawn(async move {
            tokio::time::sleep_until(expires_at).await;
            if let Some(live) = weak.upgrade() {
                let mut live = live
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if live
                    .as_ref()
                    .is_some_and(|value| value.handle == timer_handle)
                {
                    *live = None;
                    #[cfg(test)]
                    expired.notify_one();
                }
            }
        });
        if let Some(current) = live.as_mut() {
            current.timer = Some(timer.abort_handle());
        }
        Ok(handle)
    }
    pub fn take(&self, handle: &str) -> Option<HeldRecovery> {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = live.as_ref()?;
        if Instant::now() > current.expires_at {
            if let Some(current) = live.take()
                && let Some(timer) = current.timer
            {
                timer.abort();
            }
            return None;
        }
        if current.handle != handle {
            return None;
        }
        let current = live.take()?;
        if let Some(timer) = current.timer {
            timer.abort();
        }
        Some(current.recovery)
    }
    pub fn dispose_all(&self) {
        let current = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(current) = current
            && let Some(timer) = current.timer
        {
            timer.abort();
        }
    }
}
impl Drop for RecoveryHandles {
    fn drop(&mut self) {
        self.dispose_all();
    }
}

#[cfg(test)]
pub(super) fn expiry_signal(store: &RecoveryHandles) -> tokio::sync::futures::OwnedNotified {
    Arc::clone(&store.expired).notified_owned()
}

#[cfg(test)]
pub(super) fn has_live(store: &RecoveryHandles) -> bool {
    store.live.lock().expect("fixture lock").is_some()
}
