use maho_agent::harness::utils::{read_folders::*, segmented_read_view::*};
fn body(n: usize) -> String {
    format!(
        "function f{n}() {{\n{}\n}}",
        (0..6)
            .map(|i| format!("  const x{i} = \"brace }} {{ [ ]\";"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}
fn siblings() -> String {
    (0..20).map(body).collect::<Vec<_>>().join("\n")
}
fn fold(text: &str, path: &str) -> ReadFolderResult {
    SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path,
        text,
        settings: READ_FOLD_SETTINGS,
    })
}
fn view(text: &str, path: &str) -> SegmentedReadView {
    create_segmented_read_view(text, &fold(text, path))
}
#[test]
fn preserves_function_return_arrow_object_types() {
    let text=(0..20).map(|i|format!("function f{i}(): () => {{\na: string;\nb: string;\nc: string;\nd: string;\ne: string;\n}} {{\nreturn value;\n}}")).collect::<Vec<_>>().join("\n");
    let actual = create_default_read_summary(
        "return-types.ts",
        &text,
        None,
        None,
        Some(&SELECTED_READ_FOLDER),
        false,
    )
    .map_or(text.clone(), |r| r.text);
    let candidate = match view(&text, "source.ts") {
        SegmentedReadView::Summary { rendered, .. } => rendered.text,
        SegmentedReadView::NoSummary { .. } => text.clone(),
    };
    let lines: Vec<_> = text.split('\n').collect();
    for i in 0..20 {
        let signature = lines[i * 9..i * 9 + 7].join("\n");
        assert!(actual.contains(&signature));
        assert!(candidate.contains(&signature));
    }
}
#[test]
fn ambiguous_syntax_and_unsupported_languages_fall_back() {
    let sibling = siblings();
    let mut cases = Vec::new();
    for suffix in [
        "/* unfinished",
        "\"unfinished",
        "const a = `unfinished",
        "const a = [);",
        "{} /[{}]/.test('a');",
        "const a = /[\n{}]/;",
    ] {
        cases.push(("x.js", format!("{sibling}\n{suffix}"), "parse_failure"));
    }
    cases.push((
        "x.json",
        format!(
            "{}/*bad*/",
            serde_json::to_string_pretty(&vec![1; 120]).expect("fixture invariant")
        ),
        "parse_failure",
    ));
    for path in ["x.tsx", "x.py", "x.rs", "x.go", "x.jsx"] {
        cases.push((path, sibling.clone(), "unsupported_language"));
    }
    for path in ["x.md", "x.txt"] {
        cases.push((path, sibling.clone(), "prose_exempt"));
    }
    cases.push((
        "x.ts",
        format!(
            "{}\n{}",
            (0..101)
                .map(|i| format!("const x{i} = {i};"))
                .collect::<Vec<_>>()
                .join("\n"),
            body(1)
        ),
        "skeleton_exceeds_budget",
    ));
    cases.push((
        "x.ts",
        format!(
            "function big() {{\n{}\n}}",
            (0..110)
                .map(|i| format!("const x{i} = {i};"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        "visible_budget_unreachable",
    ));
    cases.push(("x.ts", vec!["let x = 1;"; 100].join("\n"), "no_elision"));
    cases.push(("x.ts", body(1), "too_short"));
    for (path, text, reason) in cases {
        assert_eq!(
            view(&text, path),
            SegmentedReadView::NoSummary {
                reason: reason.into()
            },
            "{path}: {text}"
        );
    }
    assert_eq!(
        create_segmented_read_view(&format!("{sibling}\nchanged"), &fold(&sibling, "x.ts")),
        SegmentedReadView::NoSummary {
            reason: "stale_source".into()
        }
    );
}
fn boundary(total: usize) {
    let text = format!(
        "{}\n{}",
        (0..12).map(body).collect::<Vec<_>>().join("\n"),
        (0..total - 96)
            .map(|i| format!("const top{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(text.split('\n').count(), total);
    assert_eq!(
        matches!(view(&text, "x.ts"), SegmentedReadView::Summary { .. }),
        total == 100
    );
}
#[test]
fn minimum_99() {
    boundary(99);
}
#[test]
fn minimum_100() {
    boundary(100);
}
#[test]
fn frozen_registry_constants() {
    assert_eq!(
        READ_FOLD_SETTINGS,
        ReadFoldSettings {
            min_body_lines: 4,
            min_comment_lines: 6,
            min_total_lines: 100,
            unfold_until: 50,
            unfold_limit: 100
        }
    );
    for (path, engine) in [
        ("x.ts", "raw"),
        ("x.tsx", "raw"),
        ("x.js", "wasm"),
        ("x.json", "heuristic"),
        ("x.py", "unsupported"),
        ("x.rs", "unsupported"),
        ("x.go", "unsupported"),
        ("x.md", "prose_exempt"),
        ("x.txt", "prose_exempt"),
    ] {
        assert_eq!(read_summary_engine_for_path(path), Some(engine));
    }
    assert_eq!(READ_FOLDER_SELECTION_HEAD.len(), 40);
    assert!(
        READ_FOLDER_SELECTION_HEAD
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    );
    assert_eq!(SELECTED_READ_FOLDER.id(), "measured-brace");
    assert_eq!(SELECTED_READ_FOLDER.version(), "3");
}
fn callback(language: &str) {
    let text=(0..20).map(|i|format!("function f{i}() {{\n  const regex = /[{{}}\\/]+/g;\n  const ratio = 4 / 2;\n  /* }} [ a comment */\n  const nested = `value ${{(() => {{ return `nested ${{1}}`; }})()}}`;\n  const escaped = \"\\\\\\\"}}\";\n  return ratio;\n}}")).collect::<Vec<_>>().join("\n");
    assert_eq!(
        fold(&text, &format!("file.{language}")),
        ReadFolderResult::Parsed {
            text,
            ranges: vec![]
        }
    );
}
#[test]
fn nested_callback_ts() {
    callback("ts");
}
#[test]
fn nested_callback_js() {
    callback("js");
}
#[test]
fn class_method_ranges_keep_headers() {
    let text = format!(
        "class Example {{\n{}\n}}",
        (0..20)
            .map(|i| format!(
                " method{i}() {{\n{}\n }}",
                (0..6)
                    .map(|n| format!("  let x{n} = {n};"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        fold(&text, "x.ts"),
        ReadFolderResult::Parsed {
            text,
            ranges: (0..20)
                .map(|i| ReadFoldRange {
                    start_line: i * 8 + 3,
                    end_line: i * 8 + 8,
                    children: vec![]
                })
                .collect()
        }
    );
}
#[test]
fn exact_body_comment_thresholds() {
    let text = "function a() {\n1;\n2;\n3;\n}\nfunction b() {\n1;\n2;\n3;\n4;\n}\n/*\na\nb\nc\n*/\n/*\na\nb\nc\nd\n*/\n/**\na\nb\nc\nd\n*/";
    assert_eq!(
        fold(text, "x.ts"),
        ReadFolderResult::Parsed {
            text: text.into(),
            ranges: vec![
                ReadFoldRange {
                    start_line: 7,
                    end_line: 10,
                    children: vec![]
                },
                ReadFoldRange {
                    start_line: 18,
                    end_line: 21,
                    children: vec![]
                }
            ]
        }
    );
}
fn header(header: &str) {
    let text = format!(
        "{header}\na,\nb,\nc,\nd,\ne\n{}{}",
        if header.ends_with('[') { "]" } else { "}" },
        if header.starts_with("const") {
            " = value;"
        } else {
            " from 'module';"
        }
    );
    assert_eq!(
        fold(&text, "x.ts"),
        ReadFolderResult::Parsed {
            text,
            ranges: vec![]
        }
    );
}
#[test]
fn const_object_header() {
    header("const {");
}
#[test]
fn const_array_header() {
    header("const [");
}
#[test]
fn import_header() {
    header("import {");
}
#[test]
fn export_header() {
    header("export {");
}
fn malformed(suffix: &str) {
    assert!(matches!(
        fold(&format!("{}\n{suffix}", siblings()), "x.ts"),
        ReadFolderResult::ParseFailure { .. }
    ));
}
#[test]
fn malformed_string() {
    malformed("\"unterminated");
}
#[test]
fn malformed_comment() {
    malformed("/* unclosed");
}
#[test]
fn malformed_template() {
    malformed("`unclosed");
}
#[test]
fn malformed_delimiter() {
    malformed("const a = [);");
}
#[test]
fn malformed_regex() {
    malformed("const a = /[abc/;");
}
fn ambiguous(start: &str, end: &str) {
    let text = format!("{start}\na,\nb,\nc,\nd,\ne\n{end}");
    match fold(&text, "x.ts") {
        ReadFolderResult::Parsed { ranges, .. } => assert!(ranges.is_empty()),
        ReadFolderResult::ParseFailure { .. } => {}
        ReadFolderResult::Unsupported { .. } => panic!("unsupported"),
    }
}
#[test]
fn ambiguous_parameter() {
    ambiguous("function f({", "}) {}");
}
#[test]
fn ambiguous_assignment() {
    ambiguous("({", "} = value);");
}
#[test]
fn ambiguous_return_type() {
    ambiguous("function f(): {", "} { return value; }");
}
#[test]
fn ambiguous_generic_type() {
    ambiguous("function f(): Promise<{", "}> { return value; }");
}
fn rejected(text: &str) {
    assert!(matches!(
        fold(text, "x.ts"),
        ReadFolderResult::ParseFailure { .. }
    ));
}
#[test]
fn comma_object_binding() {
    rejected("const first = 0, {\na,\nb,\nc,\nd,\ne\n} = value;");
}
#[test]
fn comma_array_binding() {
    rejected("const first = 0, [\na,\nb,\nc,\nd,\ne\n] = value;");
}
#[test]
fn generic_constraint() {
    rejected(
        "class Example<T extends {\na: string;\nb: string;\nc: string;\nd: string;\ne: string;\n}> {}",
    );
}
#[test]
fn extensionless_unsupported() {
    assert_eq!(
        fold(&siblings(), "src/js"),
        ReadFolderResult::Unsupported {
            reason: "unsupported_language".into()
        }
    );
}
#[test]
fn deep_budget_unreachable() {
    let text = format!(
        "function deep() {{\n{}{}\n{}",
        "if (true) {\n".repeat(20),
        vec!["let x = 1;"; 110].join("\n"),
        "}\n".repeat(21)
    );
    assert_eq!(
        view(&text, "x.ts"),
        SegmentedReadView::NoSummary {
            reason: "visible_budget_unreachable".into()
        }
    );
}
#[test]
fn output_overhead_counts_against_saving() {
    let text = format!("function a(){{\n;\n;\n;\n;\n}}\n{}", "\n".repeat(93));
    assert_eq!(
        view(&text, "x.ts"),
        SegmentedReadView::NoSummary {
            reason: "no_output_saving".into()
        }
    );
}
