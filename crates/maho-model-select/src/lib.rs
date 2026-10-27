//! Model selection: reference grammar, availability matching, provider id transforms, the
//! delegated-task model choice and session-start model profiles (clean-room rewrite from
//! `docs/cleanroom/specs/model-selection.md`, see `PROVENANCE.md`).

pub mod delegate;
pub mod ext;
pub mod grammar;
pub mod matching;
pub mod probe;
pub mod profile;
pub mod transforms;

pub use delegate::{ChainRung, DelegateChoice, DelegateInput, Resolved, resolve_delegate_model};
pub use ext::ModelProfileExtension;
pub use grammar::{
    ParsedModel, clamp_reasoning, normalize_model, normalize_model_id, normalize_reasoning,
    parse_model_string, parse_variant, split_reasoning_suffix,
};
pub use matching::{is_available, match_available};
pub use profile::{
    DEFAULT_PROFILE_ORDER, Exclusions, ModelProfile, ProfileResolution, ProfileRung,
    builtin_profiles, effective_profile, merge_profiles, resolve_default, resolve_profile,
};
pub use transforms::model_id_for_provider;
