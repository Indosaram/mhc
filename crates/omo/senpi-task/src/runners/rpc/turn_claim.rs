//! `rpc/turn-claim.ts`: token-owned claims; an old release cannot clear a newer claim.
use std::sync::{Arc, Mutex, PoisonError};
#[derive(Default, Clone)]
pub struct TurnClaim {
    state: Arc<Mutex<(u64, Option<u64>)>>,
}
pub fn create_turn_claim() -> TurnClaim {
    TurnClaim::default()
}
impl TurnClaim {
    pub fn claim(&self) -> Box<dyn FnOnce() + Send> {
        let token = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.0 += 1;
            state.1 = Some(state.0);
            state.0
        };
        let state = Arc::clone(&self.state);
        Box::new(move || {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.1 == Some(token) {
                state.1 = None;
            }
        })
    }
    pub fn consume(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .take()
            .is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claims_are_exclusive_and_token_owned() {
        let claims = create_turn_claim();
        assert!(!claims.consume());
        let old = claims.claim();
        let current = claims.claim();
        old();
        assert!(claims.consume());
        current();
        assert!(!claims.consume());
        let release = claims.claim();
        release();
        assert!(!claims.consume());
    }
}
