use maho_tools::{definition::AbortSignal, grep::{engine::*, rg_engine}};

#[tokio::test]
async fn ripgrep_batch_admits_ordered_files_at_cap() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["c", "a", "b"] {
        std::fs::write(directory.path().join(name), "needle\nneedle\n").unwrap();
    }
    let result = rg_engine::search(GrepEngineRequest {
        pattern: "needle".into(), cwd: directory.path().to_string_lossy().into_owned(),
        paths: vec![directory.path().to_string_lossy().into_owned()],
        mode: Some(GrepMode::Files), max_count: Some(2), ..Default::default()
    }, &AbortSignal::default()).await.unwrap();
    assert_eq!(result.file_counts.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(result.files_searched, 2);
    assert!(result.limit_reached);
}

#[tokio::test]
async fn ripgrep_batch_keeps_context_with_each_file() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["b", "a"] {
        std::fs::write(directory.path().join(name), "before\nneedle\nafter\n").unwrap();
    }
    let result = rg_engine::search(GrepEngineRequest {
        pattern: "needle".into(), cwd: directory.path().to_string_lossy().into_owned(),
        paths: vec![directory.path().to_string_lossy().into_owned()],
        context_before: Some(1), context_after: Some(1), ..Default::default()
    }, &AbortSignal::default()).await.unwrap();
    assert_eq!(result.counts.matches, Some(2));
    assert_eq!(result.matches.len(), 6);
    assert!(result.matches[..3].iter().all(|row| row.path == "a"));
    assert!(result.matches[3..].iter().all(|row| row.path == "b"));
}

#[tokio::test]
async fn ripgrep_reports_invalid_pattern_for_empty_corpus() {
    let directory = tempfile::tempdir().unwrap();
    let result = rg_engine::search(GrepEngineRequest {
        pattern: "[".into(), cwd: directory.path().to_string_lossy().into_owned(),
        paths: vec![directory.path().to_string_lossy().into_owned()], ..Default::default()
    }, &AbortSignal::default()).await;
    assert!(matches!(result, Err(GrepEngineError::InvalidPattern(_))));
}
