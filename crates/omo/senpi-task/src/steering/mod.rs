//! Steering: send / interrupt / cancel delivery to child tasks (`steering/` in TypeScript).

mod engine;
mod types;

pub use engine::SteeringEngine;
pub use types::{
    CancelAbort, CancelOptions, CancelOutcome, DestructionPort, InterruptOutcome, SendInput,
    SendOutcome, SteeringError, SteeringPort,
};

#[cfg(test)]
mod engine_tests;
