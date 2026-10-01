use maho_agent::harness::utils::{read_folders::*, segmented_read_view::*};
fn object() -> String {
    format!(
        "{{\n{}\n}}",
        ["alpha", "bravo", "charlie", "delta", "echo"]
            .map(|n| format!(" {n}: \"{n}\","))
            .join("\n")
    )
}
fn ranges(text: &str) -> Vec<ReadFoldRange> {
    match SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path: "input.js",
        text,
        settings: READ_FOLD_SETTINGS,
    }) {
        ReadFolderResult::Parsed { ranges, .. } => {
            let mut all = ranges;
            let mut i = 0;
            while i < all.len() {
                all.extend(all[i].children.clone());
                i += 1;
            }
            all
        }
        ReadFolderResult::ParseFailure { .. } => vec![],
        ReadFolderResult::Unsupported { .. } => panic!("unsupported"),
    }
}
#[test]
fn computed_method_headers_survive_direct_view() {
    let declarations: Vec<_> = (0..20)
        .map(|i| {
            format!(
                "const object{i} = {{\n [ns.factory({}) + suffix]() {{}}\n}};",
                object()
            )
        })
        .collect();
    let text = declarations.join("\n");
    assert!(ranges(&text).is_empty());
    let parsed = SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path: "input.js",
        text: &text,
        settings: READ_FOLD_SETTINGS,
    });
    let visible = match create_segmented_read_view(&text, &parsed) {
        SegmentedReadView::Summary { rendered, .. } => rendered.text,
        SegmentedReadView::NoSummary { .. } => text,
    };
    for d in declarations {
        assert!(visible.contains(&d));
    }
}
#[test]
fn recovered_targets_do_not_protect_assignment_values() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/harness_read_patterns.json"))
            .expect("fixture invariant");
    assert!(
        oracle["recovered"]["protected"]
            .as_array()
            .expect("fixture invariant")
            .contains(&serde_json::json!({"start":1,"end":7}))
    );
    assert!(
        oracle["value"]["allowed"]
            .as_array()
            .expect("fixture invariant")
            .contains(&serde_json::json!({"start":2,"end":6,"kind":"body"}))
    );
}
fn pattern(which: usize) {
    let object = object();
    let source = match which {
        0 => format!("({{ value = ns.factory({object}) }} = source);"),
        1 => format!("[value = ns.factory({object})] = source;"),
        2 => format!("({{ nested: [value = ns.factory({object})] }} = source);"),
        3 => format!("(([value = ns.factory({object})] = source));"),
        _ => unreachable!(),
    };
    for enclosing in [false, true] {
        let text = if enclosing {
            format!("function outer() {{\n{source}\nreturn value;\n}}")
        } else {
            source.clone()
        };
        let start = if enclosing { 2 } else { 1 };
        let end = start + source.split('\n').count() - 1;
        for r in ranges(&text) {
            assert!(!(r.start_line <= end && r.end_line >= start));
        }
        let lines: Vec<_> = text.split('\n').collect();
        let fabricated = vec![
            ReadSegment::Kept {
                start_line: 1,
                end_line: 1,
                text: lines[0].into(),
            },
            ReadSegment::Elided {
                start_line: 2,
                end_line: lines.len() - 1,
            },
            ReadSegment::Kept {
                start_line: lines.len(),
                end_line: lines.len(),
                text: lines[lines.len() - 1].into(),
            },
        ];
        assert!(render_segmented_read_view(&text, &fabricated).is_ok());
        assert!(2 <= end && lines.len() > start);
    }
}
#[test]
fn object_assignment_target() {
    pattern(0);
}
#[test]
fn array_assignment_target() {
    pattern(1);
}
#[test]
fn nested_assignment_target() {
    pattern(2);
}
#[test]
fn parenthesized_assignment_target() {
    pattern(3);
}
#[test]
fn fields_only_class_keeps_member_declarations() {
    let steps = (0..55)
        .map(|i| format!("ns.step{i}();"))
        .collect::<Vec<_>>()
        .join("\n");
    let fields = (0..12)
        .map(|i| format!("field{i} = ns.factory({});", object()))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("function outer() {{\n{steps}\n}}\nclass Example {{\n{fields}\n}}");
    assert_eq!(text.split('\n').count(), 143);
    assert!(
        !ranges(&text)
            .iter()
            .any(|r| r.start_line == 59 && r.end_line == 142)
    );
    let parsed = SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path: "input.js",
        text: &text,
        settings: READ_FOLD_SETTINGS,
    });
    let visible = match create_segmented_read_view(&text, &parsed) {
        SegmentedReadView::Summary { rendered, .. } => rendered.text,
        SegmentedReadView::NoSummary { .. } => text,
    };
    for i in 0..12 {
        assert!(visible.contains(&format!("field{i} = ns.factory({{")));
    }
}
