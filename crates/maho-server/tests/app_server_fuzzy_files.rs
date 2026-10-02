use maho_server::app_server::fuzzy_files::*;
#[test]
fn filename_bonus_and_unicode_source_indices_preserve_ranked_match_contract() {
    let entries = vec![FuzzyFileEntry { root:"root".into(), path:"dir/İx.txt".into(), match_type:"file".into(), file_name:"İx.txt".into() },FuzzyFileEntry { root:"root".into(), path:"ix/other".into(), match_type:"file".into(), file_name:"other".into() }];
    let results = rank_fuzzy_file_entries("ix", &entries);
    assert_eq!(results[0].path, "dir/İx.txt");
    assert_eq!(results[0].indices, vec![4,5]);
    assert_eq!(results[0].score, 1_000_045);
    assert!(rank_fuzzy_file_entries("missing", &entries).is_empty());
    assert!(rank_fuzzy_file_entries("", &entries).is_empty());
}
