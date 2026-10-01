use maho_agent::harness::utils::{read_folders::*, segmented_read_view::*};
fn fixture(index: usize) -> serde_json::Value {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/harness_read_oracle.json"))
            .expect("fixture invariant");
    data["signatures"][index].clone()
}
fn retain(index: usize) {
    let f = fixture(index);
    let signature = f["source"].as_str().expect("fixture invariant");
    let declarations: Vec<_> = (0..20)
        .map(|i| {
            signature
                .replace("Example", &format!("Example{i}"))
                .replace("choose", &format!("choose{i}"))
                .replace("value", &format!("value{i}"))
        })
        .collect();
    let text = declarations.join("\n");
    let path = format!(
        "input.{}",
        f["language"].as_str().expect("fixture invariant")
    );
    let parsed = SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path: &path,
        text: &text,
        settings: READ_FOLD_SETTINGS,
    });
    if let ReadFolderResult::Parsed { ranges, .. } = &parsed {
        let mut queue: std::collections::VecDeque<_> = ranges.iter().collect();
        while let Some(r) = queue.pop_front() {
            for i in 0..20 {
                let start = i * signature.split('\n').count() + 1;
                let end = start
                    + f["header"]
                        .as_str()
                        .expect("fixture invariant")
                        .split('\n')
                        .count()
                    + 5;
                assert!(!(r.start_line <= end && r.end_line >= start));
            }
            queue.extend(&r.children);
        }
    }
    let candidate = match create_segmented_read_view(&text, &parsed) {
        SegmentedReadView::Summary { rendered, .. } => rendered.text,
        SegmentedReadView::NoSummary { .. } => text.clone(),
    };
    let actual =
        create_default_read_summary(&path, &text, None, None, Some(&SELECTED_READ_FOLDER), false)
            .map_or(text, |r| r.text);
    for declaration in declarations {
        assert!(candidate.contains(&declaration));
        assert!(actual.contains(&declaration));
    }
}
fn oracle_rejects(index: usize) {
    let f = fixture(index);
    let start = f["header"]
        .as_str()
        .expect("fixture invariant")
        .split('\n')
        .count()
        + 1;
    let end = start + 4;
    assert!(
        !f["oracle"]["allowed"]
            .as_array()
            .expect("fixture invariant")
            .iter()
            .any(|r| r["start"] == start && r["end"] == end)
    );
    let source = f["source"].as_str().expect("fixture invariant");
    let lines: Vec<_> = source.split('\n').collect();
    assert_eq!(lines[..start - 1].join("\n"), f["header"]);
    assert_eq!(lines[end..].join("\n"), f["tail"]);
}
macro_rules! cases { ($($retain:ident,$oracle:ident,$n:expr);* $(;)?) => {$ (
    #[test] fn $retain() { retain($n); }
    #[test] fn $oracle() { oracle_rejects($n); }
)*}; }
cases! {
    class_heritage,oracle_class_heritage,0; keyof_return,oracle_keyof_return,1;
    decorator_arguments,oracle_decorator_arguments,2; default_parameter,oracle_default_parameter,3;
    multiline_implements,oracle_multiline_implements,4; binding_initializer,oracle_binding_initializer,5;
    conditional_return,oracle_conditional_return,6; mapped_return,oracle_mapped_return,7;
    typeof_indexed_return,oracle_typeof_indexed_return,8; heritage_callback,oracle_heritage_callback,9;
}
#[test]
fn rejects_enclosing_nested_header() {
    let f = fixture(0);
    let source = format!(
        "function outer() {{\n{}\nreturn Example;\n}}",
        f["source"].as_str().expect("fixture invariant")
    );
    if let ReadFolderResult::Parsed { ranges, .. } = SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path: "input.js",
        text: &source,
        settings: READ_FOLD_SETTINGS,
    }) {
        let mut queue: std::collections::VecDeque<_> = ranges.iter().collect();
        while let Some(r) = queue.pop_front() {
            assert!(!(r.start_line <= 8 && r.end_line >= 2));
            queue.extend(&r.children);
        }
    }
}
