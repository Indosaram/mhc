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

#[tokio::test]
async fn traversal_preserves_sorted_depth_first_order_ignores_and_limits() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::create_dir(root.join("node_modules")).unwrap();
    std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(root.join("a/.gitignore"), "!keep.log\n").unwrap();
    std::fs::write(root.join("a/keep.log"), "").unwrap();
    std::fs::write(root.join("a/drop.log"), "").unwrap();
    std::fs::write(root.join("z.txt"), "").unwrap();
    std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();
    let roots = vec![root.to_string_lossy().into_owned()];
    let entries = collect_fuzzy_file_entries(&roots, &Default::default()).await;
    assert_eq!(entries.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>(), vec![".gitignore", "a", "a/.gitignore", "a/keep.log", "z.txt"]);
    assert_eq!(entries[1].match_type, "directory");
    let options = FuzzyTraversalOptions { max_depth:1, ..Default::default() };
    let entries = collect_fuzzy_file_entries(&roots, &options).await;
    assert_eq!(entries.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>(), vec![".gitignore", "a", "z.txt"]);
    let options = FuzzyTraversalOptions { max_visited_entries:2, ..Default::default() };
    assert_eq!(collect_fuzzy_file_entries(&roots, &options).await.len(), 2);
    options.cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(collect_fuzzy_file_entries(&roots, &options).await.is_empty());
}
