//! Port of omo-senpi `components/model-profile` at omo `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`:
//! the `model-profile` component. It applies the active `model_profile` to the MAIN session model at
//! session start, session-scoped by construction, with a start-time emulation of the first turn's
//! request-auth resolution.
//!
//! One upstream source file maps to one native module, keeping names and order:
//! `index.ts` -> [`index`], `resolve.ts` -> [`resolve`], `request-auth.ts` -> [`request_auth`],
//! `credential-policy.ts` -> [`credential_policy`], `builtin-profiles.ts` -> [`builtin_profiles`],
//! `notice.ts` -> [`notice`].
pub mod builtin_profiles;
pub mod credential_policy;
pub mod index;
pub mod notice;
pub mod request_auth;
pub mod resolve;
pub use index::*;
