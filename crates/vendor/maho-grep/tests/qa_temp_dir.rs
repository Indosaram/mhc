use maho_grep::{search, CancelToken, GrepMatch, GrepOptions};
use std::fs;

#[test]
fn search_temp_dir_returns_expected_match_lines() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "fn main() {\n    needle();\n}\n").unwrap();
    fs::write(root.join("b.txt"), "no match\nneedle here\nneedle again\n").unwrap();
    fs::write(root.join("c.md"), "nothing\n").unwrap();
    let root = root.to_str().unwrap().to_owned();
    let options = GrepOptions {
        pattern: "needle".into(),
        paths: vec![root.clone()],
        cwd: root,
        ..GrepOptions::default()
    };

    let result = search(&options, &CancelToken::new(None)).unwrap();

    for row in &result.matches {
        println!("{}:{}:{}", row.path, row.line, row.text);
    }
    let row = |path: &str, line, column, text: &str| GrepMatch {
        path: path.into(),
        line,
        column: Some(column),
        text: text.into(),
        is_context: false,
        truncated: false,
    };
    assert_eq!(
        result.matches,
        vec![
            row("b.txt", 2, 1, "needle here"),
            row("b.txt", 3, 1, "needle again"),
            row("src/a.rs", 2, 5, "    needle();"),
        ]
    );
    assert_eq!(result.counts.matches, Some(3));
    assert_eq!(result.counts.files, 2);
    assert_eq!(result.files_searched, 3);
    assert!(result.counts.exact);
}
