//! Port of `imagegen/skill-path.ts` (senpi 903b8336).

use std::path::{Path, PathBuf};

/// The crate-relative payload location upstream resolves from its module directory.
pub fn default_skill_base_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `imagegenSkillPath`: the module-relative payload location shared by skill discovery and read
/// permissions. `None` uses the crate directory, as upstream defaults to the module directory.
pub fn imagegen_skill_path(base_dir: Option<&Path>) -> PathBuf {
    let base = base_dir.map_or_else(default_skill_base_dir, Path::to_path_buf);
    base.join("skill").join("SKILL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_skill_payload_under_base_dir() {
        let base = Path::new("base");
        assert_eq!(
            imagegen_skill_path(Some(base)),
            base.join("skill").join("SKILL.md")
        );
    }

    #[test]
    fn default_points_at_the_shipped_payload() {
        let path = imagegen_skill_path(None);
        assert!(path.ends_with(Path::new("skill").join("SKILL.md")));
        assert!(path.is_file());
    }
}
