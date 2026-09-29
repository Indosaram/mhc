use pretty_assertions::assert_eq;

use crate::tools::patch_parser::{
    PatchHunk, PatchOperation, apply_memory_patch_hunk, parse_memory_patch,
};

#[test]
fn test_rejects_malformed_patch_headers_and_footers() {
    let err1 = parse_memory_patch("invalid").unwrap_err();
    assert!(err1.message.contains("patch must start with"));

    let err2 = parse_memory_patch("*** Begin Patch\n+line\n").unwrap_err();
    assert!(err2.message.contains("patch must end with"));

    let err3 = parse_memory_patch("*** Begin Patch\n*** End Patch\ntrailing").unwrap_err();
    assert!(err3.message.contains("unexpected content after"));

    let err4 = parse_memory_patch("*** Begin Patch\n*** End Patch").unwrap_err();
    assert!(err4.message.contains("no file operations found"));
}

#[test]
fn test_parses_add_file_directives() {
    let input = "*** Begin Patch\n*** Add File: system/new.md\n+line 1\n+line 2\n*** End Patch";
    let ops = parse_memory_patch(input).expect("parse");
    assert_eq!(ops.len(), 1);
    match &ops[0] {
        PatchOperation::Add {
            target_path,
            content_lines,
        } => {
            assert_eq!(target_path, "system/new.md");
            assert_eq!(content_lines, &["line 1", "line 2"]);
        }
        _ => panic!("expected Add operation"),
    }

    let bad_add = "*** Begin Patch\n*** Add File: empty.md\n*** End Patch";
    assert!(parse_memory_patch(bad_add).is_err());
}

#[test]
fn test_parses_delete_file_directives() {
    let input = "*** Begin Patch\n*** Delete File: old.md\n*** End Patch";
    let ops = parse_memory_patch(input).expect("parse");
    assert_eq!(ops.len(), 1);
    match &ops[0] {
        PatchOperation::Delete { target_path } => {
            assert_eq!(target_path, "old.md");
        }
        _ => panic!("expected Delete operation"),
    }
}

#[test]
fn test_parses_update_file_with_move_to_and_hunks() {
    let input = "*** Begin Patch\n*** Update File: src.md\n*** Move to: dst.md\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n*** End of File\n*** End Patch";
    let ops = parse_memory_patch(input).expect("parse");
    assert_eq!(ops.len(), 1);
    match &ops[0] {
        PatchOperation::Update {
            source_path,
            target_path,
            hunks,
        } => {
            assert_eq!(source_path, "src.md");
            assert_eq!(target_path, "dst.md");
            assert_eq!(hunks.len(), 1);
            assert_eq!(hunks[0].lines, vec![" context", "-old", "+new"]);
        }
        _ => panic!("expected Update operation"),
    }
}

#[test]
fn test_applies_hunks_to_exact_content() {
    let base = "line 1\nline 2\nline 3\n";
    let hunk = PatchHunk {
        lines: vec![
            " line 1".to_string(),
            "-line 2".to_string(),
            "+line two".to_string(),
            " line 3".to_string(),
        ],
    };
    let applied = apply_memory_patch_hunk(base, &hunk, "test.md").expect("apply");
    assert_eq!(applied, "line 1\nline two\nline 3\n");

    let bad_hunk = PatchHunk {
        lines: vec![
            " missing".to_string(),
            "-old".to_string(),
            "+new".to_string(),
        ],
    };
    let err = apply_memory_patch_hunk(base, &bad_hunk, "test.md").unwrap_err();
    assert!(err.message.contains("context not found"));
    assert!(err.message.contains("Failed old/context chunk:"));
    assert!(err.message.contains("Current file content preview"));
}
