//! `omo.onboarding.state.get` / `.complete` / `.reset` (upstream `state-rpc.ts`): the one install-wide
//! onboarding state the TUI and RPC clients share. A TUI claim is `inProgress`, never `completed`;
//! completion is written when the TUI's onboarding turn finishes or a client calls `complete`, and a claim
//! released later never erases it.

use std::{io, path::PathBuf, sync::Arc};

use maho_ext_api::{ExtensionApi, ExtensionFailure, ExtensionFuture};
use serde_json::{Value, json};

use crate::claim_owner::{ClaimOwnerSeams, read_onboarding_state};

pub const ONBOARDING_STATE_GET_RPC_METHOD: &str = "omo.onboarding.state.get";
pub const ONBOARDING_STATE_COMPLETE_RPC_METHOD: &str = "omo.onboarding.state.complete";
pub const ONBOARDING_STATE_RESET_RPC_METHOD: &str = "omo.onboarding.state.reset";

pub type StateWrite = Arc<dyn Fn(&std::path::Path) -> io::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct OnboardingStateStore {
    pub state_dir: PathBuf,
    pub complete_onboarding: StateWrite,
    pub reset_onboarding: StateWrite,
    /// Clears this process's own onboarding run so its next prompt can onboard again.
    pub on_reset: Arc<dyn Fn() + Send + Sync>,
    /// Identity readers for judging a held claim's owner; defaults to the real process table.
    pub claim_owner_seams: ClaimOwnerSeams,
}

/// `{completed, completedAt, inProgress}`
pub fn read_state_response(store: &OnboardingStateStore) -> Value {
    let state = read_onboarding_state(&store.state_dir, &store.claim_owner_seams);
    json!({"completed": state.completed_at.is_some(), "completedAt": state.completed_at, "inProgress": state.in_progress})
}

pub fn register_onboarding_state_rpc(
    api: &mut ExtensionApi,
    store: OnboardingStateStore,
) -> Result<(), ExtensionFailure> {
    let handler = |store: OnboardingStateStore,
                   act: fn(&OnboardingStateStore) -> Result<(), String>|
     -> maho_ext_api::ExtensionRpcRequestHandler {
        Arc::new(move |_request| -> ExtensionFuture<'static, Value> {
            let store = store.clone();
            // The writes stay synchronous, so a complete or reset lands before the reply is even built.
            let written = act(&store);
            Box::pin(async move {
                written.map_err(ExtensionFailure::new)?;
                Ok(read_state_response(&store))
            })
        })
    };
    api.rpc_handle(
        ONBOARDING_STATE_GET_RPC_METHOD,
        handler(store.clone(), |_| Ok(())),
    )?;
    api.rpc_handle(
        ONBOARDING_STATE_COMPLETE_RPC_METHOD,
        handler(store.clone(), |store| {
            (store.complete_onboarding)(&store.state_dir).map_err(|e| e.to_string())
        }),
    )?;
    api.rpc_handle(
        ONBOARDING_STATE_RESET_RPC_METHOD,
        handler(store, |store| {
            (store.reset_onboarding)(&store.state_dir).map_err(|e| e.to_string())?;
            (store.on_reset)();
            Ok(())
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{complete_onboarding, is_onboarding_complete, reset_onboarding};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn api(dir: &std::path::Path, resets: Arc<AtomicUsize>) -> ExtensionApi {
        let mut api = ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                "state",
                "/tmp".into(),
                maho_ext_api::SourceInfo::default(),
            ),
            maho_ext_api::ExtensionSessionProfile::default(),
            maho_ext_api::EventBus::default(),
            maho_ext_api::ExtensionRuntime::default(),
        );
        register_onboarding_state_rpc(
            &mut api,
            OnboardingStateStore {
                state_dir: dir.to_owned(),
                complete_onboarding: Arc::new(complete_onboarding),
                reset_onboarding: Arc::new(reset_onboarding),
                on_reset: Arc::new(move || {
                    resets.fetch_add(1, Ordering::SeqCst);
                }),
                claim_owner_seams: ClaimOwnerSeams::default(),
            },
        )
        .expect("register state rpc");
        api
    }

    async fn call(api: &ExtensionApi, method: &str) -> Value {
        api.registered.rpc_handlers[method](Value::Null)
            .await
            .expect("state answers")
    }

    #[tokio::test]
    async fn complete_then_reset_round_trip_through_the_markers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resets = Arc::new(AtomicUsize::new(0));
        let api = api(dir.path(), Arc::clone(&resets));
        assert_eq!(
            call(&api, ONBOARDING_STATE_GET_RPC_METHOD).await,
            json!({"completed":false,"completedAt":null,"inProgress":false})
        );
        let completed = call(&api, ONBOARDING_STATE_COMPLETE_RPC_METHOD).await;
        assert_eq!(completed["completed"], true);
        assert!(completed["completedAt"].is_string() && is_onboarding_complete(dir.path()));
        let reset = call(&api, ONBOARDING_STATE_RESET_RPC_METHOD).await;
        assert_eq!(
            reset,
            json!({"completed":false,"completedAt":null,"inProgress":false})
        );
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        assert!(!is_onboarding_complete(dir.path()));
    }

    #[tokio::test]
    async fn a_held_claim_reads_in_progress_not_completed() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(crate::claim_owner::claim_onboarding_owned(
            dir.path(),
            &ClaimOwnerSeams::default()
        ));
        let api = api(dir.path(), Arc::default());
        assert_eq!(
            call(&api, ONBOARDING_STATE_GET_RPC_METHOD).await,
            json!({"completed":false,"completedAt":null,"inProgress":true})
        );
    }
}
