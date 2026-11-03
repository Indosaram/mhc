use std::collections::BTreeMap;

use maho_omo_bundled_skills::{
    BUNDLED_SKILLS_OVERRIDE_ENV, resolve_bundled_skills_dir_logged, resolve_bundled_skills_override,
};

fn env(value: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(BUNDLED_SKILLS_OVERRIDE_ENV.to_owned(), value.to_owned())])
}

#[test]
fn given_a_readable_absolute_dir_when_resolved_then_it_is_the_override_and_wins() {
    let dir = tempfile::tempdir().expect("temp");
    let value = dir.path().to_string_lossy().into_owned();
    assert_eq!(
        resolve_bundled_skills_override(&env(&value), None, None),
        Some(dir.path().to_path_buf())
    );
    assert_eq!(
        resolve_bundled_skills_dir_logged(&env(&value), None),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn given_absent_empty_relative_missing_or_file_values_when_resolved_then_there_is_no_override() {
    let dir = tempfile::tempdir().expect("temp");
    let file = dir.path().join("file");
    std::fs::write(&file, "x").expect("file");
    assert_eq!(
        resolve_bundled_skills_override(&BTreeMap::new(), None, None),
        None
    );
    assert_eq!(resolve_bundled_skills_override(&env(""), None, None), None);
    assert_eq!(
        resolve_bundled_skills_override(&env("relative/skills"), None, None),
        None
    );
    assert_eq!(
        resolve_bundled_skills_override(
            &env(&dir.path().join("missing").to_string_lossy()),
            None,
            None
        ),
        None
    );
    assert_eq!(
        resolve_bundled_skills_override(&env(&file.to_string_lossy()), None, None),
        None
    );
}
