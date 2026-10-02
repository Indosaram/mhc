use std::path::Path;
use crate::{session_continuity::Snapshot, session_reattach::BindingStore};

pub struct RestoredBindingAdmission { pub binding: Option<Snapshot>, pub transcript_available: bool }
pub async fn admit_restored_binding<F, Fut>(store: &mut BindingStore, session: &str, cwd: Option<&Path>, auth_lane: &str, verify: F) -> anyhow::Result<RestoredBindingAdmission>
where F: FnOnce(Snapshot, &Path, &str) -> Fut, Fut: std::future::Future<Output = anyhow::Result<bool>> {
    let binding = store.get(session);
    if binding.as_ref().is_none_or(|binding| binding.sent_prefix_hash.is_none()) {
        return Ok(RestoredBindingAdmission { binding, transcript_available: true });
    }
    let transcript_available = if let Some(cwd) = cwd { verify(binding.clone().expect("stored binding"), cwd, auth_lane).await? } else { false };
    if !transcript_available { store.forget(session); }
    Ok(RestoredBindingAdmission { binding, transcript_available })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn live_binding_does_not_require_transcript_verification() {
        let mut store=BindingStore::default();store.remember("s",&Snapshot::default());
        let admission=admit_restored_binding(&mut store,"s",None,"ambient",|_,_,_|async {panic!("live binding has no persisted digest")}).await.expect("admit");
        assert!(admission.transcript_available);assert!(store.get("s").is_some());
    }
    #[tokio::test]
    async fn unavailable_restored_transcript_forgets_process_binding() {
        for cwd in [None,Some(Path::new("/fixture"))] {
            let mut store=BindingStore::default();store.remember("s",&Snapshot {sdk_session_id:"sdk".into(),sent_prefix_hash:Some("digest".into()),..Default::default()});
            let admission=admit_restored_binding(&mut store,"s",cwd,"config-dir",|binding,_,lane|{assert_eq!(binding.sdk_session_id,"sdk");assert_eq!(lane,"config-dir");async {Ok(false)}}).await.expect("admit");
            assert!(!admission.transcript_available);assert!(admission.binding.is_some());assert!(store.get("s").is_none());
        }
    }
    #[tokio::test]
    async fn verified_restored_transcript_retains_process_binding() {
        let mut store=BindingStore::default();store.remember("s",&Snapshot {sent_prefix_hash:Some("digest".into()),..Default::default()});
        let admission=admit_restored_binding(&mut store,"s",Some(Path::new("/fixture")),"oauth-slots",|_,_,_|async {Ok(true)}).await.expect("admit");
        assert!(admission.transcript_available);assert!(store.get("s").is_some());
    }
}
