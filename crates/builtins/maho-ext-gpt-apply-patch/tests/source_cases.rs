use maho_ext_gpt_apply_patch::{patch_diff::create_patch_diff, parser::parse_patch, text::strip_heredoc, workspace::resolve_patch_path};
use std::path::{Path, PathBuf};

#[test]
fn pinned_diff_line_reordering_and_blank_final_line_match_source() {
    let reordered = create_patch_diff("a\nb\nc\n", "b\na\nc\n");
    assert_eq!(reordered.diff, "-1 a\n 2 b\n+2 a\n 3 c");
    assert_eq!((reordered.added, reordered.removed), (1, 1));
    let newline = create_patch_diff("a\n\na", "a\na\n");
    assert_eq!(newline.diff, " 1 a\n-2 \n-3 a\n+2 a");
    assert_eq!((newline.added, newline.removed), (1, 2));
}

#[test]
fn pinned_heredoc_regex_whitespace_and_quote_boundaries() {
    for (input, expected) in [
        ("<<PATCH\n\n\nPATCH", ""),
        ("cat\u{0085}<<PATCH\na\nPATCH", "cat\u{0085}<<PATCH\na\nPATCH"),
        ("<<PATCH\na\nPATCH\u{feff}", "a"),
        ("<<'PATCH\na\nPATCH", "a"),
        ("<<PATCH\na\nOTHER", "<<PATCH\na\nOTHER"),
    ] {
        assert_eq!(strip_heredoc(input), expected);
    }
}

#[test]
fn workspace_resolves_root_parent_empty_and_absolute_segments() {
    for (cwd, path, expected) in [
        ("/root/work", "../../../../file", "/file"),
        ("/root/work", "", "/root/work"),
        ("/root/work", "/elsewhere/./child/../file", "/elsewhere/file"),
    ] {
        assert_eq!(resolve_patch_path(Path::new(cwd), Path::new(path)), PathBuf::from(expected));
    }
}

#[test]
fn parser_preserves_move_only_and_rejects_malformed_body_without_mutation() {
    assert!(parse_patch("*** Begin Patch\n*** Update File: a\n*** Move to: b\n*** End Patch").is_ok());
    for input in [
        "*** Begin Patch\n*** Add File: a\nmissing-prefix\n*** End Patch",
        "*** Begin Patch\n*** Update File: a\n@@\n*** End Patch",
        "*** Begin Patch\n*** Rename File: a\n*** End Patch",
    ] {
        assert!(parse_patch(input).is_err());
    }
}
