use super::*;
use super::capture_tests::{current, fire, sync};
use loro::ExportMode;

fn next(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_add(0x6d2b79f5);
    let mut value = (*seed ^ (*seed >> 15)).wrapping_mul(1 | *seed);
    value = value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)) ^ value;
    value ^ (value >> 14)
}

#[test]
fn generated_update_streams_converge_in_one_thousand_delivery_orders() -> Result<(), ItemError> {
    // Given independently authored mixed register, set, removal and body updates.
    let mut updates = Vec::new();
    for peer in 1..=3 {
        let doc = create_item_doc(peer)?;
        let mut seed = u32::try_from(1000 + peer).expect("small seed");
        for step in 0..10 {
            let roll = next(&mut seed);
            let header = match roll % 5 {
                0 => format!("alpha: {}\n", roll % 100),
                1 => format!("beta: {}\n", roll.is_multiple_of(2)),
                2 => format!("gamma: word{roll}\n"),
                3 => format!("alpha: [e{}, e{}]\n", roll % 10, step),
                _ => "beta: null\n".into(),
            };
            let before = doc.oplog_vv();
            current(&doc, &format!("---\n{header}---\np{peer} step{step}\n"))?;
            updates.push(doc.export(ExportMode::updates(&before)).map_err(crdt)?);
        }
    }
    let reference = create_item_doc(9)?;
    for update in &updates { reference.import(update).map_err(crdt)?; }
    let expected = render_item(&reference)?;
    let mut seed = 7;
    // When each full causal update stream is delivered in a shuffled order.
    for order in 0..1000 {
        let mut indices: Vec<_> = (0..updates.len()).collect();
        for index in (1..indices.len()).rev() {
            let chosen = usize::try_from(next(&mut seed)).expect("u32 fits usize") % (index + 1);
            indices.swap(index, chosen);
        }
        let doc = create_item_doc(9)?;
        for index in indices { doc.import(&updates[index]).map_err(crdt)?; }
        // Then pending dependencies settle to the same canonical bytes.
        assert_eq!(render_item(&doc)?, expected, "delivery order {order}");
    }
    Ok(())
}

#[test]
fn generated_frontmatter_shapes_roundtrip_over_five_hundred_items() -> Result<(), ItemError> {
    // Given seeded mixed kinds, arbitrary key orders, invalid and body-only shapes.
    let mut seed = 42;
    let words = ["plain", "héllo", "한글", "🙂🙂", "two words", "with: colon", "42", "true", "2026-01-07"];
    for index in 0..500 {
        let roll = next(&mut seed);
        let word = words[usize::try_from(roll).expect("u32 fits usize") % words.len()];
        let quoted = yaml::source(&yaml::Value::String(word.into()));
        let body = format!("{word}\nbody {index}\n");
        let fields = [format!("description: {quoted}"), format!("count: {}", roll % 1000), format!("flag: {}", roll.is_multiple_of(2)), format!("tags: [{quoted}, 3, true]"), format!("meta:\n  a: {}", roll % 10), "note: |-\n  line one\n  line two".into()];
        let text = match roll % 10 {
            0 => format!("---\n: broken: [\n---\n{body}"),
            1 => format!("---\n- not\n- a\n- map\n---\n{body}"),
            2 => body,
            _ => {
                let mut indices: Vec<_> = (0..fields.len()).collect();
                for at in (1..indices.len()).rev() {
                    let chosen = usize::try_from(next(&mut seed)).expect("u32 fits usize") % (at + 1);
                    indices.swap(at, chosen);
                }
                let count = 1 + usize::try_from(roll).expect("u32 fits usize") % fields.len();
                let header = indices[..count].iter().map(|i| fields[*i].as_str()).collect::<Vec<_>>().join("\n");
                format!("---\n{header}\n---\n{body}")
            }
        };
        let doc = create_item_doc(1 + u64::from(roll % 3))?;
        // When captured and canonically rendered.
        current(&doc, &text)?;
        let rendered = render_item(&doc)?.expect("generated item");
        // Then re-capturing canonical output adds no operations.
        assert!(capture::capture_item(&doc, &rendered, &item_frontier(&doc), &rendered, None)?.is_none(), "item {index}");
        assert_eq!(render_item(&doc)?.as_deref(), Some(rendered.as_str()));
    }
    Ok(())
}

#[test]
fn disk_style_cross_product_preserves_model_bytes_and_idempotency() -> Result<(), ItemError> {
    for bom in [false, true] {
        for style in 0..3 {
            for front in 0..4 {
                for operation in 0..4 {
                    for seed in 0..12 {
                        // Given BOM x LF/CRLF/mixed x valid/none/broken/unclosed x edit.
                        let eol = |line: usize| match style {
                            0 => "\n",
                            1 => "\r\n",
                            _ => if (line + seed).is_multiple_of(3) { "\n" } else { "\r\n" },
                        };
                        let mut keys = ["n: 2", "k: x", "z: 1"];
                        keys.rotate_left(seed % 3);
                        let body = format!("body line{}second{}", eol(6), eol(7));
                        let bare = match front {
                            0 => format!("---{}{}{}{}{}{}{}---{}{body}", eol(0), keys[0], eol(1), keys[1], eol(2), keys[2], eol(3), eol(4)),
                            1 => body,
                            2 => format!("---{}n: [{}k: x{}---{}{body}", eol(0), eol(1), eol(2), eol(4)),
                            _ => format!("---{}n: 2{}{body}", eol(0), eol(1)),
                        };
                        let mark = if bom { "\u{feff}" } else { "" };
                        let disk = format!("{mark}{bare}");
                        let working = if front == 0 {
                            match operation {
                                0 => bare.replace("n: 2", "n: 3"),
                                1 => bare.replace(&format!("{}{}", keys[2], eol(3)), &format!("{}{}added: v{}", keys[2], eol(3), eol(5))),
                                2 => bare.replace(&format!("k: x{}", eol(keys.iter().position(|k| *k == "k: x").expect("k") + 1)), ""),
                                _ => bare.clone(),
                            }
                        } else {
                            match operation {
                                0 => bare.replace("second", "second2"),
                                1 if front == 1 => format!("---{}added: v{}---{}{bare}", eol(0), eol(1), eol(2)),
                                1 => format!("{bare}third{}", eol(8)),
                                2 => bare.replace(&format!("body line{}", eol(6)), ""),
                                _ => bare.clone(),
                            }
                        };
                        let a = create_item_doc(1)?;
                        let b = create_item_doc(2)?;
                        current(&a, &disk)?;
                        sync(&a, &b)?;
                        current(&a, &format!("{mark}{working}"))?;
                        sync(&a, &b)?;
                        // When the second device materializes over the original source.
                        let written = render_for_disk(&b, &disk)?.expect("live item");
                        // Then encoding, body, untouched spans, model and replay are stable.
                        let context = format!("bom={bom} style={style} front={front} op={operation} seed={seed}");
                        assert_eq!(written.matches('\u{feff}').count(), usize::from(bom), "{context}");
                        assert_eq!(written.starts_with('\u{feff}'), bom, "{context}");
                        assert_eq!(split_item(&written).body, split_item(&working).body, "{context}");
                        if operation == 3 { assert_eq!(written, disk, "{context}"); }
                        if front == 0 {
                            for key in keys {
                                let touched = (operation == 0 && key == "n: 2") || (operation == 2 && key == "k: x");
                                if !touched {
                                    let line = bare.split_inclusive('\n').find(|line| line.starts_with(key)).expect("source line");
                                    assert!(written.contains(line), "{context}: {line:?}");
                                }
                            }
                        }
                        let crlf = bare.matches("\r\n").count();
                        let lf = bare.matches('\n').count() - crlf;
                        let dominant = if crlf > lf || (crlf == lf && bare.find('\n').is_some_and(|at| at > 0 && bare.as_bytes()[at - 1] == b'\r')) { "\r\n" } else { "\n" };
                        let parsed = split_item(&written);
                        if parsed.fm.is_some() {
                            let header_length = written.len() - parsed.body.len();
                            for line in written[..header_length].split_inclusive('\n') {
                                if !disk.split_inclusive('\n').any(|old| old == line) {
                                    assert!(line.ends_with(dominant), "{context}: {line:?}");
                                    if dominant == "\n" { assert!(!line.ends_with("\r\n"), "{context}"); }
                                }
                            }
                        }
                        assert_eq!(render_for_disk(&b, &written)?.as_deref(), Some(written.as_str()), "{context}");
                        assert!(capture::capture_item(&b, &render::render_doc_content(&b)?, &item_frontier(&b), &written, None)?.is_none(), "{context}");
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn three_device_seeded_capture_model_converges_without_spliced_values() -> Result<(), Box<dyn std::error::Error>> {
    for initial in 0..2000 {
        // Given three deterministic peers with isolated device receipts and stale disks.
        let dir = tempfile::tempdir()?;
        let devices: Vec<_> = (1..=3).map(|peer| ItemDevice { peer, state_dir: dir.path().join(peer.to_string()) }).collect();
        let docs = devices.iter().map(create_device_item_doc).collect::<Result<Vec<_>, _>>()?;
        let mut disks = [String::new(), String::new(), String::new()];
        let mut saved: [(String, ItemFrontier); 3] = std::array::from_fn(|_| (String::new(), vec![]));
        let mut written_values: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = std::collections::BTreeMap::new();
        let mut markers = Vec::new();
        let mut model = (1..=3).map(property_model::Model::new).collect::<Result<Vec<_>, _>>()?;
        let mut seed = initial;
        // When user edits, double fires, syncs and materialization crashes interleave.
        for step in 0..22 {
            let roll = next(&mut seed);
            let index = usize::try_from(roll % 3).expect("three peers");
            match (roll / 3) % 6 {
                0 | 1 => {
                    let marker = format!("m{initial}x{step}q");
                    let parsed = split_item(&disks[index]);
                    let mut fields = parsed.fm.unwrap_or_default();
                    let mut body = parsed.body;
                    let hostile = ["", " ", "\n", "\r\n", "\t", " # ", ": ", " |- ", " > ", "\n---\n", " long long long long long long long long long long long long long long long long long long long long", " 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴 긴"];
                    let extra = hostile[usize::try_from(next(&mut seed)).expect("u32 fits usize") % hostile.len()];
                    let field = next(&mut seed) % 5;
                    if field < 2 {
                        let key = if field == 0 { "n" } else { "description" };
                        let value = if field == 0 && step % 2 == 0 { format!("{}", roll % 1000) } else { yaml::source(&yaml::Value::String(format!("w{extra}{marker}"))) };
                        written_values.entry(key.into()).or_default().insert(value.clone());
                        fields.retain(|entry| entry.key != key);
                        fields.push(FmEntry { key: key.into(), value: FmValue::Reg { value } });
                    } else if field < 4 {
                        let old = fields.iter().find(|entry| entry.key == "tags").map(|entry| entry.value.clone());
                        let mut elements = match old { Some(FmValue::Set { elements, .. }) => elements, _ => vec![] };
                        let operation = next(&mut seed) % 4;
                        fields.retain(|entry| entry.key != "tags");
                        let replacement = if operation < 2 || elements.is_empty() {
                            let element = yaml::source(&yaml::Value::String(format!("{}{marker}", extra.trim_start())));
                            markers.push(marker.clone());
                            written_values.entry("tags".into()).or_default().insert(element.clone());
                            elements.push(element);
                            None
                        } else if operation == 2 {
                            elements.remove(usize::try_from(roll).expect("u32 fits usize") % elements.len());
                            None
                        } else {
                            elements.clear();
                            let value = yaml::source(&yaml::Value::String(format!("k: {marker}")));
                            written_values.entry("tags".into()).or_default().insert(value.clone());
                            Some(FmValue::Reg { value })
                        };
                        if let Some(value) = replacement {
                            fields.push(FmEntry { key: "tags".into(), value });
                        } else if !elements.is_empty() {
                            let flow = yaml::source(&yaml::Value::Seq(elements.iter().map(|element| yaml::parse(element).expect("element source").value).collect()));
                            written_values.entry("tags".into()).or_default().insert(flow.clone());
                            fields.push(FmEntry { key: "tags".into(), value: FmValue::Set { elements, flow } });
                        }
                    } else {
                        let body_marker = format!("b{initial}x{step}q");
                        markers.push(body_marker.clone());
                        body.push_str(&format!("{body_marker}\n"));
                    }
                    let entries = fields.iter().filter_map(|entry| match &entry.value {
                        FmValue::Reg { value } => Some((entry.key.clone(), yaml::parse(value).expect("canonical source").value)),
                        FmValue::Set { elements, .. } => Some((entry.key.clone(), yaml::Value::Seq(elements.iter().map(|v| yaml::parse(v).expect("element source").value).collect()))),
                        FmValue::Remove => None,
                    }).collect::<Vec<_>>();
                    disks[index] = if entries.is_empty() { body } else { format!("---\n{}\n---\n{body}", frontmatter::emit_frontmatter(&entries)) };
                }
                2 => {
                    model[index].capture(&saved[index].1, &disks[index])?;
                    fire(&devices[index], &docs[index], &saved[index].0, &saved[index].1, &disks[index])?;
                    assert_eq!(render_item(&docs[index])?.unwrap_or_default(), model[index].render(), "seed {initial} step {step}");
                    model[index].capture(&saved[index].1, &disks[index])?;
                    fire(&devices[index], &docs[index], &saved[index].0, &saved[index].1, &disks[index])?;
                    assert_eq!(render_item(&docs[index])?.unwrap_or_default(), model[index].render(), "seed {initial} step {step}");
                }
                3 => { model[index].sync(&model[(index + 1) % 3])?; sync(&docs[index], &docs[(index + 1) % 3])?; }
                4 | 5 => {
                    model[index].capture(&saved[index].1, &disks[index])?;
                    fire(&devices[index], &docs[index], &saved[index].0, &saved[index].1, &disks[index])?;
                    let text = render_item(&docs[index])?.unwrap_or_default();
                    assert_eq!(text, model[index].render(), "seed {initial} step {step}");
                    if !roll.is_multiple_of(4) {
                        model[index].materialize(&saved[index].1, roll % 4 != 1);
                        record_materialized(&devices[index], "item.md", &docs[index], &saved[index].1, &text)?;
                        disks[index] = text;
                        if roll % 4 != 1 { saved[index] = (disks[index].clone(), item_frontier(&docs[index])); }
                    }
                }
                _ => unreachable!("modulo six"),
            }
        }
        for index in 0..3 {
            model[index].capture(&saved[index].1, &disks[index])?;
            fire(&devices[index], &docs[index], &saved[index].0, &saved[index].1, &disks[index])?;
        }
        for a in 0..3 { for b in 0..3 { if a != b { model[a].sync(&model[b])?; sync(&docs[a], &docs[b])?; } } }
        // Then all peers and all six delivery orders converge to whole written sources.
        let rendered = render_item(&docs[0])?;
        for doc in &docs {
            assert_eq!(render_item(doc)?, rendered, "seed {initial}");
            for (key, values) in active_registers(doc) {
                for value in values { assert!(written_values.get(&key).is_some_and(|written| written.contains(&value)), "seed {initial}: {key}={value}"); }
            }
            assert!(doc.oplog_vv().iter().all(|(peer, _)| [1, 2, 3].contains(peer)));
        }
        let text = rendered.unwrap_or_default();
        let expected_text = model[0].render();
        if initial == 33 {
            assert_eq!(expected_text, "---\ndescription: \"w |- m33x19q / wm33x5q\"\nn: 943\n---\nb33x6q\n");
        }
        if initial == 1201 {
            assert_eq!(expected_text, "---\ndescription: \"w m1201x20q / w m1201x7q\"\ntags: \"[ m1201x15q ] / [ m1201x21q ]\"\n---\nb1201x11q\n");
        }
        assert_eq!(text, expected_text, "seed {initial}: complete expected state");
        for marker in markers {
            let expected = expected_text.matches(&marker).count();
            assert_eq!(text.matches(&marker).count(), expected, "seed {initial}: {marker}");
        }
        let exports = docs.iter().map(|doc| doc.export(ExportMode::all_updates()).map_err(crdt)).collect::<Result<Vec<_>, _>>()?;
        for order in [[0,1,2], [0,2,1], [1,0,2], [1,2,0], [2,0,1], [2,1,0]] {
            let fresh = create_item_doc(9)?;
            for index in order { fresh.import(&exports[index]).map_err(crdt)?; }
            assert_eq!(render_item(&fresh)?.unwrap_or_default(), text, "seed {initial}");
        }
        for index in 0..3 {
            record_materialized(&devices[index], "item.md", &docs[index], &saved[index].1, &text)?;
            assert!(capture_on_device(&devices[index], "item.md", &docs[index], &saved[index].0, &saved[index].1, &text, &[])?.is_none(), "seed {initial}");
        }
    }
    Ok(())
}
