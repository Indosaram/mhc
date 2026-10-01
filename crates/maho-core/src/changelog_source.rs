//! Port of senpi packages/coding-agent/src/core/changelog-source.ts.

use std::path::Path;

use crate::brand::{BrandProfile, injected_brand_profile};
use crate::config::get_package_dir;
use crate::engine_build_identity::ENGINE_VERSION;
use crate::paths::lexical_resolve;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogSource {
    pub id: String,
    pub path: String,
    pub version: Option<String>,
    pub rewrite_links: bool,
}

/// getChangelogPath: the CHANGELOG.md beside the running package.
pub fn get_changelog_path() -> String {
    lexical_resolve(&Path::new(&get_package_dir()).join("CHANGELOG.md").to_string_lossy())
}

fn brand_source(brand: &BrandProfile) -> ChangelogSource {
    match &brand.changelog {
        None => ChangelogSource {
            id: brand.name.to_lowercase(),
            path: get_changelog_path(),
            version: None,
            rewrite_links: false,
        },
        Some(configured) => ChangelogSource {
            id: brand.name.to_lowercase(),
            path: configured.path.clone(),
            version: configured.version.clone(),
            rewrite_links: false,
        },
    }
}

pub fn resolve_changelog_source() -> ChangelogSource {
    match injected_brand_profile() {
        Some(brand) => brand_source(&brand),
        None => ChangelogSource {
            id: "engine".to_owned(),
            path: get_changelog_path(),
            version: Some(ENGINE_VERSION.to_owned()),
            rewrite_links: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_source_points_at_the_package_changelog() {
        let source = resolve_changelog_source();
        assert!(source.path.ends_with("CHANGELOG.md"));
        if source.id == "engine" {
            assert!(source.rewrite_links);
            assert_eq!(source.version.as_deref(), Some(ENGINE_VERSION));
        }
    }

    #[test]
    fn a_branded_profile_uses_its_configured_changelog() {
        let brand = BrandProfile {
            name: "omo".into(),
            command: None,
            display_version: None,
            config_dir: ".omo".into(),
            flat_layout: false,
            env_prefix: "OMO".into(),
            user_agent: "omo".into(),
            originator: None,
            update: None,
            changelog: Some(crate::brand::BrandChangelog { path: "/tmp/CHANGELOG.md".into(), version: Some("1.2.3".into()) }),
        };
        let source = brand_source(&brand);
        assert_eq!(source.id, "omo");
        assert_eq!(source.path, "/tmp/CHANGELOG.md");
        assert_eq!(source.version.as_deref(), Some("1.2.3"));
        assert!(!source.rewrite_links);
    }

    #[test]
    fn a_brand_without_a_configured_changelog_falls_back_to_the_package_path() {
        let brand = BrandProfile {
            name: "tau".into(),
            command: None,
            display_version: None,
            config_dir: ".tau".into(),
            flat_layout: false,
            env_prefix: "TAU".into(),
            user_agent: "tau".into(),
            originator: None,
            update: None,
            changelog: None,
        };
        let source = brand_source(&brand);
        assert_eq!(source.id, "tau");
        assert!(source.version.is_none());
        assert!(source.path.ends_with("CHANGELOG.md"));
    }
}
