//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-account-lifetime.ts`.
//!
//! Checks whether an account principal is still current (D11 S3-4) and arms revocation timers.

use crate::mcp_presenter_account::{
    AccountPrincipal, PresenterAccountAuthority, PresenterAccountRefusal,
};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

pub fn account_lapse(
    principal: &AccountPrincipal,
    authority: Option<&dyn PresenterAccountAuthority>,
) -> Option<PresenterAccountRefusal> {
    let host_account = authority.and_then(|a| a.host_account());
    let Some(host_acc) = host_account else {
        return Some(PresenterAccountRefusal::PresenterAccountUnavailable);
    };
    if host_acc != principal.acc {
        return Some(PresenterAccountRefusal::PresenterAccountMismatch);
    }
    if now_secs() < principal.attested_until as f64 {
        None
    } else {
        Some(PresenterAccountRefusal::AttestationExpired)
    }
}

#[derive(Default)]
pub struct AttestationExpiry {
    timers: HashMap<String, JoinHandle<()>>,
}

impl AttestationExpiry {
    pub fn arm(
        &mut self,
        principal_id: &str,
        attested_until: i64,
        expire: impl FnOnce() + Send + 'static,
    ) {
        self.clear(principal_id);
        let delay_ms = ((attested_until as f64 * 1000.0) - (now_secs() * 1000.0)).max(0.0);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;
            expire();
        });
        self.timers.insert(principal_id.to_owned(), handle);
    }

    pub fn clear(&mut self, principal_id: &str) {
        if let Some(handle) = self.timers.remove(principal_id) {
            handle.abort();
        }
    }

    pub fn dispose(&mut self) {
        for (_, handle) in self.timers.drain() {
            handle.abort();
        }
    }
}

impl Drop for AttestationExpiry {
    fn drop(&mut self) {
        self.dispose();
    }
}
