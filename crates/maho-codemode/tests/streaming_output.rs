use maho_codemode::output::streaming_output::*;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn tail_totals_are_exact() {
    let mut sink = OutputSink::new(OutputSinkOptions { spill_threshold: 5, ..Default::default() });
    sink.push("abc", 0).await.unwrap(); sink.push("def", 0).await.unwrap();
    let summary = sink.dump(None).await.unwrap();
    assert_eq!(summary.output, "bcdef"); assert!(summary.truncated);
    assert_eq!((summary.total_lines, summary.total_bytes, summary.output_lines, summary.output_bytes), (1, 6, 1, 5));
}

#[tokio::test]
async fn head_and_tail_keep_one_gap() {
    let mut sink = OutputSink::new(OutputSinkOptions { spill_threshold: 6, head_bytes: 6, ..Default::default() });
    sink.push(&(0..12).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n"), 0).await.unwrap();
    let summary = sink.dump(None).await.unwrap();
    assert!(summary.output.starts_with("L0\n")); assert!(summary.output.ends_with("L11"));
    assert_eq!(summary.output.matches("elided").count(), 1);
    assert!(summary.elided_bytes.unwrap() > 0); assert!(summary.elided_lines.unwrap() > 0);
}

#[tokio::test]
async fn split_columns_clamp_once() {
    let mut sink = OutputSink::new(OutputSinkOptions { max_columns: 4, spill_threshold: 100, ..Default::default() });
    sink.push("ab", 0).await.unwrap(); sink.push("cdefgh\nnext", 0).await.unwrap();
    let summary = sink.dump(None).await.unwrap();
    assert_eq!(summary.output, "abcd…\nnext"); assert_eq!(summary.column_truncated_lines, Some(1));
    assert_eq!(summary.column_dropped_bytes, Some(4)); assert_eq!(summary.total_bytes, 13);
}

async fn mirrored(input: &str, chunks: &[&str], max_columns: usize, spill_threshold: usize) -> std::io::Result<OutputSummary> {
    let dir = tempfile::tempdir()?; let path = dir.path().join("artifact.log");
    let mut sink = OutputSink::new(OutputSinkOptions { artifact_path: Some(path.clone()), max_columns, spill_threshold, ..Default::default() });
    for chunk in chunks { sink.push(chunk, 0).await?; }
    let summary = sink.dump(None).await?;
    assert_eq!(tokio::fs::read_to_string(&path).await?, input);
    assert_eq!(summary.artifact_id.as_ref(), Some(&path)); Ok(summary)
}

#[tokio::test]
async fn column_cap_mirrors_raw_before_spill() {
    let input = format!("{}\n", "x".repeat(50));
    let summary = mirrored(&input, &[&input], 8, 50 * 1024).await.unwrap();
    assert_eq!(summary.output, "xxxxxxxx…\n"); assert_eq!(summary.column_dropped_bytes, Some(42));
}

#[tokio::test]
async fn split_utf8_is_preserved_in_artifact() {
    let input = format!("{}\n", "가".repeat(20));
    let summary = mirrored(&input, &[&input[..21], &input[21..]], 8, 50 * 1024).await.unwrap();
    assert!(summary.truncated); assert_eq!(summary.column_truncated_lines, Some(1));
    assert!(!summary.output.contains('\u{fffd}'));
}

#[tokio::test]
async fn narrow_gap_mirrors_raw() {
    let input = format!("{}\n", "x".repeat(770));
    let summary = mirrored(&input, &[&input], 768, 50 * 1024).await.unwrap();
    assert_eq!(summary.column_dropped_bytes, Some(2));
}

#[tokio::test]
async fn throttled_preview_flushes_pending_data() {
    let chunks = Arc::new(Mutex::new(Vec::new())); let output = chunks.clone();
    let mut sink = OutputSink::new(OutputSinkOptions { chunk_throttle_ms: 60_000, on_chunk: Some(Arc::new(move |chunk| output.lock().unwrap().push(chunk.to_owned()))), ..Default::default() });
    for chunk in ["a", "b", "c"] { sink.push(chunk, 100_000).await.unwrap(); }
    assert_eq!(sink.dump(None).await.unwrap().output, "abc");
    assert_eq!(*chunks.lock().unwrap(), ["a", "bc"]);
}

#[tokio::test]
async fn threshold_spill_keeps_complete_stream() {
    let summary = mirrored("abcdef", &["abc", "def"], 0, 5).await.unwrap();
    assert!(summary.truncated);
}

#[tokio::test]
async fn exact_threshold_does_not_create_artifact() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("small.log");
    let mut sink = OutputSink::new(OutputSinkOptions { artifact_path: Some(path.clone()), spill_threshold: 5, ..Default::default() });
    sink.push("abcde", 0).await.unwrap(); let summary = sink.dump(None).await.unwrap();
    assert!(!path.exists()); assert!(!summary.truncated); assert!(summary.artifact_id.is_none());
}

#[tokio::test]
async fn dump_is_idempotent() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("idempotent.log");
    let mut sink = OutputSink::new(OutputSinkOptions { artifact_path: Some(path.clone()), spill_threshold: 3, ..Default::default() });
    sink.push("abcdef", 0).await.unwrap(); let first = sink.dump(Some("first")).await.unwrap();
    assert_eq!(sink.dump(Some("second")).await.unwrap(), first);
    assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), "abcdef");
}

#[tokio::test]
async fn empty_output_has_zero_totals() {
    let mut sink = OutputSink::new(OutputSinkOptions::default());
    sink.push("", 0).await.unwrap(); let summary = sink.dump(None).await.unwrap();
    assert_eq!(summary.total_lines, 0); assert_eq!(summary.output_bytes, 0);
}

#[tokio::test]
async fn artifact_errors_propagate() {
    let dir = tempfile::tempdir().unwrap();
    let mut sink = OutputSink::new(OutputSinkOptions { artifact_path: Some(dir.path().into()), spill_threshold: 0, ..Default::default() });
    assert!(sink.push("x", 0).await.is_err());
}
