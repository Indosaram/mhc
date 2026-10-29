use super::*;
use super::capture_tests::{capture, current, sync};
use loro::ExportMode;

#[test]
fn empty_documents_and_body_only_shapes_render_without_frontmatter() -> Result<(), ItemError> {
    // Given bodies that are not valid frontmatter mappings.
    for text in ["---\n---\nbody\n", "---\n- just\n- a\n- list\n---\nkept whole\n", "---\nx: !custom thing\n---\nbody\n"] {
        let doc = create_item_doc(1)?;
        assert!(!is_live(&doc));
        assert!(render_item(&doc)?.is_none());
        // When captured as an item.
        current(&doc, text)?;
        // Then no bytes are lost to classification.
        assert_eq!(render_item(&doc)?.as_deref(), Some(text));
    }
    Ok(())
}

#[test]
fn keys_keep_first_write_order_and_body_edits_are_codepoint_safe() -> Result<(), ItemError> {
    // Given keys deliberately not in lexical order and a Unicode body.
    let doc = create_item_doc(1)?;
    current(&doc, "---\nsecond: 2\nfirst: 1\n---\na🙂b\n")?;
    // When Unicode characters are inserted and deleted.
    current(&doc, "---\nsecond: 2\nfirst: 1\n---\na🙂🙂b\n")?;
    current(&doc, "---\nsecond: 2\nfirst: 1\n---\n🙂b\n")?;
    // Then only the requested codepoints change and key order remains historical.
    assert_eq!(render_item(&doc)?.as_deref(), Some("---\nsecond: 2\nfirst: 1\n---\n🙂b\n"));
    Ok(())
}

#[test]
fn concurrent_kind_matrix_converges_and_recaptures_as_a_fixed_point() -> Result<(), ItemError> {
    let sources = ["alpha", "true", "3", "'3'", "'a: b'", "'a / b / c'", "[ x, y ]", "{ a: 1 }", "\"l1\\nl2\""];
    for left in sources {
        for right in sources {
            // Given independent first writes, with both possible peer-kind orders.
            let a = create_item_doc(1)?;
            let b = create_item_doc(2)?;
            current(&a, &format!("---\nk: {left}\n---\n"))?;
            current(&b, &format!("---\nk: {right}\n---\n"))?;
            let ua = a.export(ExportMode::all_updates()).map_err(crdt)?;
            let ub = b.export(ExportMode::all_updates()).map_err(crdt)?;
            let ab = create_item_doc(9)?;
            let ba = create_item_doc(10)?;
            // When updates arrive in opposite orders.
            ab.import(&ua).map_err(crdt)?;
            ab.import(&ub).map_err(crdt)?;
            ba.import(&ub).map_err(crdt)?;
            ba.import(&ua).map_err(crdt)?;
            // Then both whole sources survive, with valid, stable YAML.
            let rendered = render_item(&ab)?.expect("live merged item");
            assert_eq!(render_item(&ba)?.as_deref(), Some(rendered.as_str()), "{left} + {right}");
            assert!(split_item(&rendered).fm.is_some(), "{left} + {right}");
            assert!(capture::capture_item(&ab, &rendered, &item_frontier(&ab), &rendered, None)?.is_none(), "{left} + {right}");
            let history = history::analyze_history(&ab);
            let value = state::combine_key(&state::read_key_state(&ab, "k"), "k", &history.keys["k"], &history).expect("combined value");
            match value {
                state::CombinedValue::Reg { text, .. } => {
                    let expected: std::collections::BTreeSet<_> = [left, right]
                        .into_iter()
                        .flat_map(|source| {
                            frontmatter::sequence_elements(source).unwrap_or_else(|| {
                                vec![yaml::source(&yaml::parse(source).expect("fixture source").value)]
                            })
                        })
                        .collect();
                    assert_eq!(text, expected.into_iter().collect::<Vec<_>>().join(" / "), "{left} + {right}");
                }
                state::CombinedValue::Set { elements } => {
                    for source in [left, right] {
                        for element in frontmatter::sequence_elements(source).unwrap_or_else(|| vec![yaml::source(&yaml::parse(source).expect("fixture source").value)]) {
                            assert!(elements.contains(&element), "{left} + {right}: {element}");
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn register_conflicts_keep_types_and_utf8_source_order() -> Result<(), ItemError> {
    for (left, right, joined) in [("3", "'3'", "\"3\" / 3"), ("2", "10", "10 / 2"), ("\"a\\nb\"", "'|-'", "\"a\\nb\" / \"|-\"")] {
        // Given concurrent replacements of a shared register.
        let a = create_item_doc(1)?;
        let b = create_item_doc(2)?;
        current(&a, "---\nk: 1\n---\n")?;
        sync(&a, &b)?;
        current(&a, &format!("---\nk: {left}\n---\n"))?;
        current(&b, &format!("---\nk: {right}\n---\n"))?;
        // When synced.
        sync(&a, &b)?;
        // Then canonical sources, not their decoded text, determine identity and ordering.
        assert_eq!(render::frontmatter_entries(&a)?, vec![("k".into(), yaml::Value::Atom(yaml::double_quoted(joined)))]);
        assert_eq!(render_item(&a)?, render_item(&b)?);
    }
    Ok(())
}

#[test]
fn register_values_joining_a_set_keep_their_yaml_types() -> Result<(), ItemError> {
    for source in ["3", "true", "{ a: 1 }", "[ 3, true ]"] {
        // Given a set-kind first writer and a concurrent register writer.
        let a = create_item_doc(1)?;
        let b = create_item_doc(2)?;
        current(&a, "---\nother: 1\n---\n")?;
        sync(&a, &b)?;
        current(&a, "---\nother: 1\nk: [x]\n---\n")?;
        current(&b, &format!("---\nother: 1\nk: {source}\n---\n"))?;
        // When the kinds merge.
        sync(&a, &b)?;
        // Then numeric, boolean, mapping and list sources are not stringified.
        let fields = render::frontmatter_entries(&a)?;
        let (_, yaml::Value::Seq(elements)) = fields.iter().find(|(key, _)| key == "k").expect("k") else { panic!("set") };
        let parsed = yaml::parse(source)?.value;
        let additions = match parsed { yaml::Value::Seq(values) => values, value => vec![value] };
        assert_eq!(elements.first(), Some(&yaml::Value::String("x".into())));
        assert_eq!(&elements[1..], additions.as_slice());
    }
    Ok(())
}

#[test]
fn retyping_number_as_string_is_a_real_write() -> Result<(), ItemError> {
    // Given a numeric register.
    let doc = create_item_doc(1)?;
    current(&doc, "---\nn: 3\n---\n")?;
    // When the user quotes the same digits.
    current(&doc, "---\nn: '3'\n---\n")?;
    // Then the value is a string and recapture settles.
    assert_eq!(split_item(&render_item(&doc)?.expect("live")).fm.expect("fields")[0].value, FmValue::Reg { value: "\"3\"".into() });
    let text = render_item(&doc)?.expect("live");
    assert!(capture::capture_item(&doc, &text, &item_frontier(&doc), &text, None)?.is_none());
    Ok(())
}

#[test]
fn hostile_strings_and_block_forms_remain_typed_and_byte_stable() -> Result<(), ItemError> {
    let mut forms: Vec<String> = ["---\nk: |\n  line one\n  line two\n---\n", "---\nk: >\n  folded line\n  continues here\n---\n", "---\nk: |-\n  kept\n\n  blank above\n---\n", "---\nk: 'it''s: #x'\n---\n"].into_iter().map(str::to_owned).collect();
    let mut strings = vec!["word ".repeat(40), "한국어 설명 문장 ".repeat(20), "a\nb".into(), "a\r\nb".into(), "\tlead tab".into(), " lead".into(), "trail ".into(), "x # y".into(), "k: v".into(), "|-".into(), ">".into(), "a\n".into(), "#start".into(), "- dash".into(), "a\n---\nb".into()];
    for space in ['\u{a0}', '\u{feff}', '\u{3000}', '\u{2003}', '\u{2009}', '\u{200b}', '\u{2028}', '\u{85}', '\u{1680}', '\u{202f}', '\u{2029}', '\u{7f}', '\u{9f}'] {
        strings.extend([format!("{space}v"), format!("v{space}"), format!("{space}v{space}"), format!("a{space}b")]);
    }
    for value in strings {
        // Given a scalar containing characters YAML may fold or reject literally.
        let source = yaml::source(&yaml::Value::String(value.clone()));
        assert_eq!(yaml::parse(&source)?.value, yaml::Value::String(value));
        forms.push(format!("---\nk: {source}\n---\nbody\n"));
    }
    for form in forms {
        let doc = create_item_doc(1)?;
        let expected = split_item(&form).fm.expect("fixture frontmatter");
        // When capture and render are repeated.
        current(&doc, &form)?;
        let first = render_item(&doc)?.expect("live");
        for _ in 0..5 {
            // Then parsed values and canonical bytes remain unchanged with no new ops.
            assert_eq!(split_item(&first).fm.as_ref(), Some(&expected));
            assert!(capture::capture_item(&doc, &first, &item_frontier(&doc), &first, None)?.is_none());
            assert_eq!(render_item(&doc)?.as_deref(), Some(first.as_str()));
        }
    }
    Ok(())
}

#[test]
fn disk_splices_preserve_untouched_bytes_and_inline_lost_aliases() -> Result<(), ItemError> {
    let anchored = "---\nbase: &x hello\n# about other\nother: *x # alias note\nn: 2 # keep\n---\nbody\n";
    let cases = [
        (anchored, "---\nother: hello\nn: 2\n---\nbody\n", "---\n# about other\nother: hello\nn: 2 # keep\n---\nbody\n"),
        (anchored, "---\nbase: hello\nother: own\nn: 2\n---\nbody\n", "---\nbase: &x hello\n# about other\nother: own\nn: 2 # keep\n---\nbody\n"),
        (anchored, "---\nbase: hello\nother: hello\nn: 3\n---\nbody\n", "---\nbase: &x hello\n# about other\nother: *x # alias note\nn: 3\n---\nbody\n"),
        ("---\n# about n\nn: 2 # old note\nkeep: 'x' # untouched\n---\nbody\n", "---\nn: 3\nkeep: x\n---\nbody\n", "---\n# about n\nn: 3\nkeep: 'x' # untouched\n---\nbody\n"),
        ("---\nd: >\n  long folded\n  text here\nn: 2\nflow: {a: 1,   b: 2}\n# trailing comment\n---\nbody\n", "---\nd: \"long folded text here\\n\"\nn: 3\nflow: {a: 1, b: 2}\nadded: yes-new\n---\nbody\n", "---\nd: >\n  long folded\n  text here\nn: 3\nflow: {a: 1,   b: 2}\nadded: yes-new\n# trailing comment\n---\nbody\n"),
    ];
    for (disk, working, expected) in cases {
        // Given a styled file and a second device's edit.
        let a = create_item_doc(1)?;
        let b = create_item_doc(2)?;
        current(&a, disk)?;
        sync(&a, &b)?;
        current(&b, working)?;
        sync(&a, &b)?;
        // When materialized onto the original style.
        let written = render_for_disk(&a, disk)?.expect("live");
        // Then only changed spans are rewritten, and a second write is a no-op.
        assert_eq!(written, expected);
        assert_eq!(render_for_disk(&a, &written)?.as_deref(), Some(written.as_str()));
    }
    Ok(())
}

#[test]
fn disk_fallback_handles_broken_yaml_and_top_level_merge_keys() -> Result<(), ItemError> {
    for disk in ["---\nn: [unclosed\n---\nbody\n", "---\nd: &m { a: 1 }\n<<: *m\nn: 2\n---\nbody\n"] {
        // Given unpatchable disk frontmatter and a valid authoritative document.
        let doc = create_item_doc(1)?;
        current(&doc, "---\nd: { a: 1 }\na: 1\nn: 3\n---\nbody\n")?;
        // When written to disk.
        let written = render_for_disk(&doc, disk)?;
        // Then the canonical model wins over unsafe source splicing.
        assert_eq!(written, render_item(&doc)?);
    }
    Ok(())
}

#[test]
fn stale_identical_register_edit_does_not_duplicate_canonical_sources() -> Result<(), ItemError> {
    // Given a typed baseline whose value has already changed.
    let doc = create_item_doc(1)?;
    let seed = "---\nflag: true\n---\n";
    current(&doc, seed)?;
    let at = item_frontier(&doc);
    current(&doc, "---\nflag: 'q: r'\n---\n")?;
    // When the same edit arrives from the stale baseline.
    capture(&doc, seed, &at, "---\nflag: 'q: r'\n---\n")?;
    // Then the whole register source appears once and is stable.
    let rendered = render_item(&doc)?.expect("live");
    assert_eq!(rendered.matches("q: r").count(), 1);
    assert!(capture::capture_item(&doc, &rendered, &item_frontier(&doc), &rendered, None)?.is_none());
    Ok(())
}

#[test]
fn concurrent_set_additions_follow_lamport_then_numeric_peer_order() -> Result<(), ItemError> {
    // Given one seed element and equal-Lamport additions by two peers.
    let a = create_item_doc(1)?;
    let b = create_item_doc(2)?;
    let c = create_item_doc(10)?;
    current(&a, "---\ntags: [b]\n---\n")?;
    sync(&a, &b)?;
    sync(&a, &c)?;
    current(&b, "---\ntags: [b, a]\n---\n")?;
    current(&c, "---\ntags: [b, c]\n---\n")?;
    // When both additions reach all replicas.
    sync(&a, &c)?;
    sync(&a, &b)?;
    sync(&b, &c)?;
    // Then numeric peer 2 precedes 10, not lexical peer order.
    let expected = "---\ntags:\n  - b\n  - a\n  - c\n---\n";
    for doc in [&a, &b, &c] { assert_eq!(render_item(doc)?.as_deref(), Some(expected)); }
    Ok(())
}

#[test]
fn nested_merge_alias_rewrites_preserve_unrelated_source() -> Result<(), ItemError> {
    // Given an anchored map used by a nested merge and an untouched comment.
    let disk = "---\nd: &m { a: 1 }\ne: { <<: *m, b: 2 } # merged\nz: 9 # keep\n---\nbody\n";
    let doc = create_item_doc(1)?;
    current(&doc, disk)?;
    // When the anchor definition changes without changing its already resolved user.
    current(&doc, "---\nd: { a: 5 }\ne: { a: 1, b: 2 }\nz: 9\n---\nbody\n")?;
    let written = render_for_disk(&doc, disk)?.expect("live");
    // Then the dependent map is inlined while unrelated bytes remain.
    assert_eq!(written, "---\nd: { a: 5 }\ne: { a: 1, b: 2 }\nz: 9 # keep\n---\nbody\n");
    Ok(())
}
