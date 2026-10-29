//! Reflection and memory-maintenance reminder generation.

#[expect(
    clippy::module_inception,
    reason = "mirrors the TS reminders/reminders.ts layout"
)]
pub mod reminders;

pub use reminders::{
    ConflictState, PushFailure, Reminder, ReminderKind, RemindersState, RepoStatusSnapshot,
    create_reminders_state, generate_reminders, reminder_kind_for,
};
