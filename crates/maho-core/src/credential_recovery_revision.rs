//! Port of senpi packages/coding-agent/src/core/credential-recovery-revision.ts.

use crate::auth_storage::AuthStorage;

/// The retry-fallback recovery revision of `provider` (senpi#4099): the `loginId` the last sign-in
/// stored, or `""` when none did. An unreadable store returns `None`.
pub fn credential_recovery_revision(
    storage: Option<&AuthStorage>,
    provider: &str,
) -> Option<String> {
    let storage = storage?;
    // Metadata read reloads another process's write without resolving key references.
    if storage.get_load_error().is_some() {
        return None;
    }
    let _ = storage.list();
    if storage.get_load_error().is_some() {
        return None;
    }
    let login_id = storage
        .get(provider)
        .and_then(|v| {
            v.get("loginId")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    Some(login_id)
}
