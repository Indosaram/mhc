//! The persona prompt a facts run is given, and the on-disk override that replaces it.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-persona-asset.md`. The prompt shapes what the model treats as a
//! storable fact, so a missing override must not silently fall back to something unrelated or empty.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The persona files an operator can override on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PersonaAssetKind {
    Reflection,
    Dream,
    Facts,
    Kibitzer,
}

impl PersonaAssetKind {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Reflection => "reflection-persona.md",
            Self::Dream => "dream-persona.md",
            Self::Facts => "facts-persona.md",
            Self::Kibitzer => "kibitzer-persona.md",
        }
    }

    /// The built-in prompt, compiled in so a run works with no assets directory at all.
    pub fn builtin(self) -> &'static str {
        match self {
            Self::Reflection => {
                include_str!("../../omo/memory-core/src/reflection/assets/reflection-persona.md")
            }
            Self::Dream => {
                include_str!("../../omo/memory-core/src/reflection/assets/dream-persona.md")
            }
            Self::Facts => include_str!("../../omo/memory-core/src/facts/facts-persona.md"),
            Self::Kibitzer => include_str!("../../omo/memory-core/src/recall/kibitzer-persona.md"),
        }
    }
}

static CACHE: Mutex<Option<BTreeMap<PathBuf, String>>> = Mutex::new(None);

/// Read an override from `assets_dir`, else the built-in prompt.
///
/// The cache is keyed by the full path, so pointing two identities at different assets directories cannot
/// serve one identity's prompt to the other. Content is cached rather than the whole map replaced because the
/// assets directory is read on every run and the file does not change mid-process.
pub fn load_persona(kind: PersonaAssetKind, assets_dir: Option<&Path>) -> std::io::Result<String> {
    let Some(dir) = assets_dir else {
        return Ok(kind.builtin().to_owned());
    };
    let path = dir.join(kind.filename());
    if let Some(cached) = cached(&path) {
        return Ok(cached);
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        // A missing override is the normal case, not an error: most runs use the built-in prompt.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(kind.builtin().to_owned());
        }
        Err(error) => return Err(error),
    };
    remember(&path, &content);
    Ok(content)
}

fn cached(path: &Path) -> Option<String> {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|entries| entries.get(path).cloned())
}

fn remember(path: &Path, content: &str) {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(BTreeMap::new)
        .insert(path.to_path_buf(), content.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "maho-persona-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create");
        dir
    }

    #[test]
    fn every_kind_has_a_builtin_prompt() {
        for kind in [
            PersonaAssetKind::Reflection,
            PersonaAssetKind::Dream,
            PersonaAssetKind::Facts,
            PersonaAssetKind::Kibitzer,
        ] {
            assert!(!kind.builtin().trim().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn filenames_are_distinct() {
        let names: Vec<&str> = [
            PersonaAssetKind::Reflection,
            PersonaAssetKind::Dream,
            PersonaAssetKind::Facts,
            PersonaAssetKind::Kibitzer,
        ]
        .iter()
        .map(|kind| kind.filename())
        .collect();
        let unique: std::collections::BTreeSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn no_assets_dir_means_the_builtin() {
        let loaded = load_persona(PersonaAssetKind::Facts, None).expect("builtin");
        assert_eq!(loaded, PersonaAssetKind::Facts.builtin());
    }

    #[test]
    fn a_missing_override_falls_back_to_the_builtin() {
        let dir = assets_dir("missing");
        let loaded = load_persona(PersonaAssetKind::Facts, Some(&dir)).expect("builtin");
        assert_eq!(loaded, PersonaAssetKind::Facts.builtin());
    }

    #[test]
    fn an_override_replaces_the_builtin() {
        let dir = assets_dir("override");
        std::fs::write(dir.join("facts-persona.md"), "custom prompt\n").expect("write");
        let loaded = load_persona(PersonaAssetKind::Facts, Some(&dir)).expect("override");
        assert_eq!(loaded, "custom prompt\n");
    }

    #[test]
    fn an_override_for_one_kind_does_not_leak_to_another() {
        let dir = assets_dir("leak");
        std::fs::write(dir.join("facts-persona.md"), "facts only\n").expect("write");
        assert_eq!(
            load_persona(PersonaAssetKind::Facts, Some(&dir)).expect("override"),
            "facts only\n"
        );
        assert_eq!(
            load_persona(PersonaAssetKind::Kibitzer, Some(&dir)).expect("builtin"),
            PersonaAssetKind::Kibitzer.builtin()
        );
    }

    #[test]
    fn two_asset_dirs_do_not_share_cached_content() {
        let first = assets_dir("cache-a");
        let second = assets_dir("cache-b");
        std::fs::write(first.join("dream-persona.md"), "from a\n").expect("write a");
        std::fs::write(second.join("dream-persona.md"), "from b\n").expect("write b");
        assert_eq!(
            load_persona(PersonaAssetKind::Dream, Some(&first)).expect("a"),
            "from a\n"
        );
        assert_eq!(
            load_persona(PersonaAssetKind::Dream, Some(&second)).expect("b"),
            "from b\n"
        );
    }

    #[test]
    fn an_unreadable_override_is_an_error_not_a_silent_fallback() {
        let dir = assets_dir("unreadable");
        std::fs::create_dir_all(dir.join("facts-persona.md")).expect("dir");
        let error = load_persona(PersonaAssetKind::Facts, Some(&dir)).expect_err("error");
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
