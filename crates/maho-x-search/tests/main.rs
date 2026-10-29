//! Single integration-test binary: one module per area of docs/cleanroom/specs/x-search.md §8.

mod support;

#[path = "availability.rs"]
mod availability;
#[path = "call.rs"]
mod call;
#[path = "normalize.rs"]
mod normalize;
#[path = "output.rs"]
mod output;
#[path = "params.rs"]
mod params;
#[path = "skill.rs"]
mod skill;
