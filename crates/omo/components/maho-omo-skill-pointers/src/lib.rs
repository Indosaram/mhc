//! Maho Omo Skill Pointers component.
//! File-for-file port of upstream `packages/omo-senpi/src/components/skill-pointers`.

pub mod index;
pub mod strip_quoted_regions;

pub use index::*;
pub use strip_quoted_regions::{
    mask_negated_invocations, strip_quoted_regions, strip_quoted_spans,
    NOT_AFTER_IDENTIFIER, NOT_BEFORE_PATH, NOT_INTO_IDENTIFIER,
};
