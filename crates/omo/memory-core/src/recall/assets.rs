//! Kibitzer persona asset loader (latest `recall/assets/assets.ts`).

use std::path::Path;
use std::sync::Mutex;

use crate::fs::resilient;
use crate::personas::manifest::{PersonaAssetKind, persona_asset_filename};

static PERSONA_CACHE: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);

/// Verbatim resident persona embedded for native consumers without staged assets.
pub const KIBITZER_PERSONA_MARKDOWN: &str = include_str!("kibitzer-persona.md");

pub const fn kibitzer_persona() -> &'static str {
    KIBITZER_PERSONA_MARKDOWN
}

/// Loads the kibitzer persona markdown from `assets_dir`, caching the content per process.
pub fn load_kibitzer_persona(assets_dir: &Path) -> std::io::Result<String> {
    let path = assets_dir.join(persona_asset_filename(PersonaAssetKind::Kibitzer));
    let key = path.to_string_lossy().into_owned();
    {
        let guard = PERSONA_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entries) = guard.as_ref()
            && let Some((_, content)) = entries.iter().find(|(cached, _)| cached == &key)
        {
            return Ok(content.clone());
        }
    }
    let content = resilient::read_to_string(&path)?;
    let mut guard = PERSONA_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entries = guard.get_or_insert_with(Vec::new);
    entries.push((key, content.clone()));
    Ok(content)
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
