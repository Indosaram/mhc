//! Port of omo-senpi `components/x-search/skill.test.ts` at latest `455dee62`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn parse_front_matter(markdown: &str) -> BTreeMap<String, String> {
    let lines: Vec<&str> = markdown.lines().collect();
    assert_eq!(lines.first(), Some(&"---"), "SKILL.md must open with a YAML front matter fence");

    let end_idx = lines[1..]
        .iter()
        .position(|&l| l == "---")
        .expect("SKILL.md front matter is not closed")
        + 1;

    let mut fields = BTreeMap::new();
    for line in &lines[1..end_idx] {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        let mut value = value.trim().to_string();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = value[1..value.len() - 1].to_string();
        }
        fields.insert(key, value);
    }
    fields
}

#[test]
fn test_skill_front_matter_name_pinned_to_x_search() {
    let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skill/SKILL.md");
    let content = fs::read_to_string(&skill_path).expect("read SKILL.md");
    let fm = parse_front_matter(&content);

    assert_eq!(fm.get("name").map(String::as_str), Some("x-search"));
}

#[test]
fn test_skill_front_matter_description_length_under_1024() {
    let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skill/SKILL.md");
    let content = fs::read_to_string(&skill_path).expect("read SKILL.md");
    let fm = parse_front_matter(&content);

    let desc = fm.get("description").expect("description field");
    assert!(!desc.is_empty());
    assert!(desc.len() < 1024);
}

#[test]
fn test_skill_front_matter_description_is_ascii_only() {
    let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skill/SKILL.md");
    let content = fs::read_to_string(&skill_path).expect("read SKILL.md");
    let fm = parse_front_matter(&content);

    let desc = fm.get("description").expect("description field");
    assert!(desc.is_ascii());
}
