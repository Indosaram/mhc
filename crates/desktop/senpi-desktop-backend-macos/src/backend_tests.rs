//! The backend wiring of the user input watch (omo v5.1.52
//! `backend.rs`/`backend/capabilities.rs`): a fresh backend reports the
//! watch as not started, and the trait reads presses from the watch.

use senpi_desktop_core::backend::Backend;
use senpi_desktop_core::types::{DisplaySelector, InterventionWatch};
use senpi_desktop_core::user_input::UserPresses;

use super::MacosBackend;

#[test]
fn a_fresh_backend_reports_its_intervention_watch_as_not_started() {
    // Given
    let mut backend = MacosBackend::new(DisplaySelector::All).expect("backend");
    // When
    let inherent = backend.capabilities();
    let through_trait = Backend::capabilities(&mut backend);
    // Then
    assert_eq!(
        inherent.intervention_watch,
        Some(InterventionWatch::NotStarted)
    );
    assert_eq!(
        through_trait.intervention_watch,
        Some(InterventionWatch::NotStarted)
    );
    assert_eq!(
        Backend::take_user_presses(&mut backend),
        UserPresses::default()
    );
}
