//! People cards and contact models for memory-core.

pub mod format;

pub use format::{
    CardEntry, ObservationEntry, ObservationGroup, PeopleCard, PeopleCardParseResult, PeopleLimits,
    VALID_PREFIXES, VALID_SECTIONS, is_reserved_slug, parse_comment_fields, parse_people_card,
    resolve_slug_collision, sanitize_person_slug, serialize_people_card,
};
