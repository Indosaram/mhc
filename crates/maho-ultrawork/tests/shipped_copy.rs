//! `contract/` is canonical; the shipped skill must carry it byte for byte.

use std::fs;
use std::path::{Path, PathBuf};

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn skill_dir() -> PathBuf {
    crate_dir().join("../../assets/omo-latest/skills/ultrawork")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn reference_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(crate_dir().join("contract/references"))
        .expect("contract references")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn skill_body_is_the_core_contract_followed_by_the_read_on_demand_list() {
    let core = read(&crate_dir().join("contract/core.md"));
    let skill = read(&skill_dir().join("SKILL.md"));
    let body = skill
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(_, body)| body.trim_start_matches('\n'))
        .expect("front matter");
    let tail = body
        .strip_prefix(core.trim_end())
        .expect("skill body starts with the core contract");
    let list = tail
        .trim_start()
        .strip_prefix("---")
        .and_then(|rest| rest.trim_start().strip_prefix("## Read on demand"))
        .expect("read-on-demand section follows the contract");
    let mut listed: Vec<String> = list
        .lines()
        .filter(|line| line.starts_with("- "))
        .map(|line| {
            let (_, tail) = line.rsplit_once("`references/").expect("reference path");
            tail.trim_end_matches('`').to_owned()
        })
        .collect();
    listed.sort();
    assert_eq!(listed, reference_names());
}

#[test]
fn each_shipped_reference_equals_its_contract_source() {
    for name in reference_names() {
        let source = read(&crate_dir().join("contract/references").join(&name));
        let shipped = read(&skill_dir().join("references").join(&name));
        assert_eq!(shipped, source, "{name}");
    }
}

#[test]
fn shipped_reference_set_has_no_extra_files() {
    let mut shipped: Vec<String> = fs::read_dir(skill_dir().join("references"))
        .expect("shipped references")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    shipped.sort();
    assert_eq!(shipped, reference_names());
}
