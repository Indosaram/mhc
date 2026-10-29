//! Port of omo-senpi `components/model-profile` at omo `v5.1.47`:
//! the `model-profile` component. It applies the active `model_profile` to the MAIN session model at
//! session start, session-scoped by construction, with a start-time emulation of the first turn's
//! request-auth resolution.
//!
//! One upstream source file maps to one native module, keeping names and order:
//! `index.ts` -> [`index`], `resolve.ts` -> [`resolve`], `request-auth.ts` -> [`request_auth`],
//! `credential-policy.ts` -> [`credential_policy`], `builtin-profiles.ts` -> [`builtin_profiles`],
//! `notice.ts` -> [`notice`], `models-json.ts` -> [`models_json`],
//! `custom-providers.ts` -> [`custom_providers`], `describe.ts` -> [`describe`].
pub mod builtin_profiles;
pub mod credential_policy;
pub mod custom_providers;
pub mod default_profile;
pub mod describe;
pub mod index;
pub mod models_json;
pub mod notice;
pub mod request_auth;
pub mod resolve;
pub use index::*;
