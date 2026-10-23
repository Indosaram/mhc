//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "claim_takeover_race.rs"]
mod claim_takeover_race;
#[path = "component.rs"]
mod component;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "process_claim.rs"]
mod process_claim;
#[path = "state.rs"]
mod state;
#[path = "state_release.rs"]
mod state_release;
