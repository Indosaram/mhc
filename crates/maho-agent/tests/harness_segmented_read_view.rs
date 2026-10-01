use maho_agent::harness::utils::read_folders::*;
use maho_agent::harness::utils::segmented_read_view::*;

fn source(total: usize) -> String {
    (0..total)
        .map(|n| format!("source line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn range(start_line: usize, end_line: usize, children: Vec<ReadFoldRange>) -> ReadFoldRange {
    ReadFoldRange {
        start_line,
        end_line,
        children,
    }
}
fn view(text: &str, ranges: Vec<ReadFoldRange>) -> SegmentedReadView {
    create_segmented_read_view(
        text,
        &ReadFolderResult::Parsed {
            text: text.into(),
            ranges,
        },
    )
}
fn coverage(text: &str, segments: &[ReadSegment]) -> Vec<ReadLineRange> {
    let lines: Vec<_> = text.split('\n').collect();
    let mut next = 1;
    let mut reconstructed = Vec::new();
    let mut omitted = Vec::new();
    for segment in segments {
        let range = segment.range();
        assert_eq!(range.start_line, next);
        assert!(range.end_line >= next);
        let original = &lines[next - 1..range.end_line];
        match segment {
            ReadSegment::Kept { text, .. } => assert_eq!(*text, original.join("\n")),
            ReadSegment::Elided { .. } => omitted.push(range),
        }
        reconstructed.extend_from_slice(original);
        next = range.end_line + 1;
    }
    assert_eq!(next, lines.len() + 1);
    assert_eq!(reconstructed.join("\n"), text);
    omitted
}
#[test]
fn bfs_preserves_source_and_reaches_visible_budget() {
    let methods = (0..20)
        .map(|i| {
            format!(
                " method{i}() {{\n{}\n }}",
                (0..6)
                    .map(|n| format!("  let x{n} = {n};"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let class = format!("class Example {{\n{methods}\n}}");
    let siblings = (0..20)
        .map(|i| {
            format!(
                "function f{i}() {{\n{}\n}}",
                (0..6)
                    .map(|n| format!("  let x{n} = \"brace }} {{ [ ]\";"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let json = serde_json::to_string_pretty(
        &(0..25)
            .map(|n| serde_json::json!({"n":n,"a":[1,2,3,4,5],"s":"escaped \" } {"}))
            .collect::<Vec<_>>(),
    )
    .expect("fixture invariant");
    for (path, text) in [
        ("source.ts", class),
        ("source.js", siblings),
        ("source.json", json),
    ] {
        let parsed = SELECTED_READ_FOLDER.fold(ReadFolderInput {
            path,
            text: &text,
            settings: READ_FOLD_SETTINGS,
        });
        let result = create_segmented_read_view(&text, &parsed);
        let SegmentedReadView::Summary {
            segments,
            visible_source_lines,
            rendered,
        } = result
        else {
            panic!("{result:?}");
        };
        assert!((50..=100).contains(&visible_source_lines));
        assert_eq!(coverage(&text, &segments), rendered.elided_ranges);
        assert!(rendered.text.len() < text.len());
        assert_eq!(
            rendered
                .text
                .split('\n')
                .filter(|line| *line == "…")
                .count(),
            rendered.elided_ranges.len()
        );
        assert_eq!(
            rendered.footer.rereads,
            rendered
                .elided_ranges
                .iter()
                .map(|r| Reread {
                    offset: r.start_line,
                    limit: r.end_line - r.start_line + 1
                })
                .collect::<Vec<_>>()
        );
        if path == "source.ts" {
            for i in 0..20 {
                assert!(
                    rendered
                        .text
                        .split('\n')
                        .any(|line| line == format!(" method{i}() {{"))
                );
            }
        }
    }
}
#[test]
fn exposes_siblings_before_grandchildren() {
    let text = source(210);
    let result = view(
        &text,
        vec![
            range(2, 91, vec![range(12, 81, vec![range(17, 76, vec![])])]),
            range(102, 191, vec![range(112, 181, vec![])]),
        ],
    );
    let SegmentedReadView::Summary {
        visible_source_lines,
        rendered,
        ..
    } = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(visible_source_lines, 50);
    assert_eq!(
        rendered.elided_ranges,
        [
            ReadLineRange {
                start_line: 12,
                end_line: 81
            },
            ReadLineRange {
                start_line: 102,
                end_line: 191
            }
        ]
    );
}
#[test]
fn uses_fifo_frontier() {
    let text = source(190);
    let result = view(
        &text,
        vec![
            range(2, 91, vec![range(12, 81, vec![])]),
            range(99, 188, vec![range(109, 178, vec![])]),
        ],
    );
    let SegmentedReadView::Summary {
        visible_source_lines,
        rendered,
        ..
    } = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(visible_source_lines, 50);
    assert_eq!(
        rendered.elided_ranges,
        [
            ReadLineRange {
                start_line: 12,
                end_line: 81
            },
            ReadLineRange {
                start_line: 109,
                end_line: 178
            }
        ]
    );
}
#[test]
fn preserves_crlf_whitespace_unicode_and_terminal_empty_line() {
    let text = "signature {  \r\nomit\r\n}\t\r\n… actual source\r\n";
    let segments = vec![
        ReadSegment::Kept {
            start_line: 1,
            end_line: 1,
            text: "signature {  \r".into(),
        },
        ReadSegment::Elided {
            start_line: 2,
            end_line: 2,
        },
        ReadSegment::Kept {
            start_line: 3,
            end_line: 5,
            text: "}\t\r\n… actual source\r\n".into(),
        },
    ];
    let result = render_segmented_read_view(text, &segments).expect("fixture invariant");
    assert_eq!(result.elided_ranges, coverage(text, &segments));
    assert!(
        result
            .text
            .starts_with("signature {  \r\n…\n}\t\r\n… actual source\r\n")
    );
}
fn invalid(segments: &[ReadSegment]) {
    assert_eq!(
        render_segmented_read_view("a\nb\nc", segments),
        Err(InvalidReadSegmentsError)
    );
}
#[test]
fn rejects_altered_lines() {
    invalid(&[ReadSegment::Kept {
        start_line: 1,
        end_line: 3,
        text: "altered".into(),
    }]);
}
#[test]
fn rejects_zero_start() {
    invalid(&[ReadSegment::Elided {
        start_line: 0,
        end_line: 3,
    }]);
}
#[test]
fn rejects_end_past_source() {
    invalid(&[ReadSegment::Elided {
        start_line: 1,
        end_line: 4,
    }]);
}
#[test]
fn rejects_incomplete_coverage() {
    invalid(&[ReadSegment::Elided {
        start_line: 1,
        end_line: 1,
    }]);
}
#[test]
fn rejects_overlapping_segments() {
    invalid(&[
        ReadSegment::Elided {
            start_line: 1,
            end_line: 2,
        },
        ReadSegment::Elided {
            start_line: 2,
            end_line: 3,
        },
    ]);
}
#[test]
fn rejects_unrepresentable_endpoint() {
    invalid(&[ReadSegment::Elided {
        start_line: 1,
        end_line: usize::MAX,
    }]);
}
#[test]
fn rejects_crossing_duplicate_and_stale_ranges() {
    let text = source(120);
    for ranges in [
        vec![range(2, 80, vec![]), range(70, 119, vec![])],
        vec![range(2, 80, vec![range(2, 80, vec![])])],
        vec![range(2, 119, vec![range(2, 119, vec![])])],
    ] {
        assert_eq!(
            view(&text, ranges),
            SegmentedReadView::NoSummary {
                reason: "invalid_ranges".into()
            }
        );
    }
    assert_eq!(
        create_segmented_read_view(
            &format!("{text}\nnew"),
            &ReadFolderResult::Parsed {
                text,
                ranges: vec![]
            }
        ),
        SegmentedReadView::NoSummary {
            reason: "stale_source".into()
        }
    );
}
#[test]
fn skips_oversized_step_but_exposes_safe_sibling() {
    let text = source(300);
    let result = view(
        &text,
        vec![
            range(2, 181, vec![]),
            range(195, 294, vec![range(215, 274, vec![])]),
        ],
    );
    let SegmentedReadView::Summary {
        segments,
        visible_source_lines,
        rendered,
    } = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(visible_source_lines, 60);
    assert_eq!(
        rendered.elided_ranges,
        [
            ReadLineRange {
                start_line: 2,
                end_line: 181
            },
            ReadLineRange {
                start_line: 215,
                end_line: 274
            }
        ]
    );
    coverage(&text, &segments);
}
#[test]
fn deterministic_without_retaining_caller_state() {
    let text = (0..20)
        .map(|i| format!("function f{i}() {{\n1;\n2;\n3;\n4;\n5;\n6;\n}}"))
        .collect::<Vec<_>>()
        .join("\n");
    let fold = |text: &str| {
        SELECTED_READ_FOLDER.fold(ReadFolderInput {
            path: "source.ts",
            text,
            settings: READ_FOLD_SETTINGS,
        })
    };
    let first = create_segmented_read_view(&text, &fold(&text));
    let other = format!("{text}\n\"unterminated");
    assert!(matches!(
        fold(&other),
        ReadFolderResult::ParseFailure { .. }
    ));
    assert_eq!(first, create_segmented_read_view(&text, &fold(&text)));
}
