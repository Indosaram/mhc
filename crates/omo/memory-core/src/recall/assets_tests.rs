use std::path::PathBuf;

use super::load_kibitzer_persona;

#[test]
fn given_the_source_asset_when_loaded_through_the_loader_then_it_equals_the_real_file_and_is_non_empty()
{
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("recall");
    let loaded = load_kibitzer_persona(&dir).expect("the kibitzer persona loads");
    assert_eq!(loaded, include_str!("kibitzer-persona.md"));
    assert!(!loaded.trim().is_empty());
}

#[test]
fn given_a_missing_asset_directory_when_loaded_then_the_error_surfaces() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("recall")
        .join("no-such-assets");
    assert!(load_kibitzer_persona(&dir).is_err());
}
