//! Port of `test/suite/gpt-apply-patch-line-endings.test.ts` and
//! `test/suite/regressions/2656-apply-patch-main-merge.test.ts` (senpi 903b8336): only the lines a
//! patch changes are rewritten, so CRLF and mixed-ending files keep their endings.

use maho_ext_gpt_apply_patch::apply::apply_patch_detailed;
use std::path::{Path, PathBuf};

fn workspace_with(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join(name);
    std::fs::write(&file, content).unwrap();
    (directory, file)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn keeps_crlf_on_every_line_when_a_crlf_file_gets_a_change_and_an_insertion() {
    let (cwd, file) = workspace_with("lines.txt", "one\r\ntwo\r\nthree\r\n");
    let result = apply_patch_detailed(
        cwd.path(),
        "*** Begin Patch\n*** Update File: lines.txt\n@@\n-one\n+ONE\n two\n+between\n three\n*** End Patch\n",
    )
    .await
    .unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(read(&file), "ONE\r\ntwo\r\nbetween\r\nthree\r\n");
}

#[tokio::test]
async fn leaves_untouched_lines_of_a_mixed_ending_file_with_their_own_endings() {
    let (cwd, file) = workspace_with("lines.txt", "one\r\ntwo\rthree\nfour\r\n");
    let result = apply_patch_detailed(
        cwd.path(),
        "*** Begin Patch\n*** Update File: lines.txt\n@@\n one\n two\n-three\n+THREE\n four\n*** End Patch\n",
    )
    .await
    .unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(read(&file), "one\r\ntwo\rTHREE\r\nfour\r\n");
}

#[tokio::test]
async fn leaves_a_context_line_matched_only_after_trimming_exactly_as_it_was() {
    let (cwd, file) = workspace_with("code.ts", "const a = 1;   \r\nconst b = 2;\r\n");
    let result = apply_patch_detailed(
        cwd.path(),
        "*** Begin Patch\n*** Update File: code.ts\n@@\n const a = 1;\n-const b = 2;\n+const b = 3;\n*** End Patch\n",
    )
    .await
    .unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(read(&file), "const a = 1;   \r\nconst b = 3;\r\n");
}

#[tokio::test]
async fn keeps_an_lf_file_lf_and_still_ends_it_with_a_newline() {
    let (cwd, file) = workspace_with("lines.txt", "alpha\nbeta");
    let result = apply_patch_detailed(
        cwd.path(),
        "*** Begin Patch\n*** Update File: lines.txt\n@@\n-beta\n+BETA\n*** End Patch\n",
    )
    .await
    .unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(read(&file), "alpha\nBETA\n");
}

#[tokio::test]
async fn reports_a_context_mismatch_on_a_crlf_file_and_leaves_its_bytes_untouched() {
    let original = "one\r\ntwo\r\n";
    let (cwd, file) = workspace_with("lines.txt", original);
    let result = apply_patch_detailed(
        cwd.path(),
        "*** Begin Patch\n*** Update File: lines.txt\n@@\n-missing\n+x\n*** End Patch\n",
    )
    .await
    .unwrap();
    assert!(result.applied_files.is_empty());
    let failed: Vec<_> = result
        .failures
        .iter()
        .map(|failure| failure.file_path.as_str())
        .collect();
    assert_eq!(failed, ["lines.txt"]);
    assert_eq!(read(&file), original);
}

#[tokio::test]
async fn counts_only_the_changed_line_for_a_one_line_change_to_a_crlf_file() {
    let (cwd, file) = workspace_with("sample.txt", "keep\r\nbefore\r\nkeep too\r\n");
    let result = apply_patch_detailed(
        cwd.path(),
        "*** Begin Patch\n*** Update File: sample.txt\n@@\n-before\n+after\n*** End Patch",
    )
    .await
    .unwrap();
    let preview = &result.details.applied_operations[0].preview;
    assert_eq!((preview.added, preview.removed), (1, 1));
    assert_eq!(read(&file), "keep\r\nafter\r\nkeep too\r\n");
}

#[tokio::test]
async fn applies_a_duplicate_context_addition_on_a_crlf_file_and_counts_one_addition() {
    let (cwd, file) = workspace_with("code.ts", "const a = 1;\r\nconst b = 2;\r\n");
    let patch = "*** Begin Patch\n*** Update File: code.ts\n@@\n const a = 1;\n+const a = 1;\n const b = 2;\n*** End Patch";
    let result = apply_patch_detailed(cwd.path(), patch).await.unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(
        read(&file),
        "const a = 1;\r\nconst a = 1;\r\nconst b = 2;\r\n"
    );
    let parsed = maho_ext_gpt_apply_patch::parser::parse_patch(patch).unwrap();
    let maho_ext_gpt_apply_patch::types::ParsedPatch::Update { chunks, .. } = &parsed[0] else {
        panic!("expected update hunk")
    };
    let counts = chunks.iter().fold((0, 0), |(added, removed), chunk| {
        (added + chunk.added_count, removed + chunk.removed_count)
    });
    assert_eq!(counts, (1, 0));
}
