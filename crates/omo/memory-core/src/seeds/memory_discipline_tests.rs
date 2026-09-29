use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_memory_discipline_skill_path_when_checked_then_is_valid_relative_path() {
    assert_eq!(
        MEMORY_DISCIPLINE_SKILL_PATH,
        "skills/memory-discipline/SKILL.md"
    );
    let parsed = crate::memfs::frontmatter::parse_memory_file(MEMORY_DISCIPLINE_SKILL_CONTENT);
    assert!(parsed.is_ok());
    assert_eq!(
        parsed.unwrap().frontmatter.description,
        "This skill should be used when deciding whether and where to save memory. It routes knowledge between memory notes, skills, and people records, specifies the card and observation formats, and explains when to reach for /reflect, /dream, /search, and /people."
    );
}
