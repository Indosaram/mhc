//! Embedded persona assets for reflection and dream agents.

#[allow(clippy::module_inception)] // mirrors the TS reflection/assets/assets.ts layout
pub mod assets;

pub use assets::{
    DREAM_PERSONA_MARKDOWN, REFLECTION_PERSONA_MARKDOWN, ReflectionPersona,
    ReflectionPersonaSection, load_dream_persona, load_reflection_persona, parse_sections,
};
