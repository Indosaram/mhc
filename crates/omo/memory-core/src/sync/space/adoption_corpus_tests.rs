use super::corpus_support::{Facts, Random, block, sole_registers};
use super::tests::{commit, seed, state};
use super::*;
use crate::sync::item::{self, render::render_doc_content, yaml::Value};

struct Header {
    text: String,
    keys: Vec<(String, Value)>,
}

fn header(random: &mut Random, source: usize, iteration: usize) -> Header {
    if random.chance(0.03) {
        return Header { text: String::new(), keys: Vec::new() };
    }
    let mut fields = Vec::new();
    let mut keys = Vec::new();
    for key in 0..4 {
        if random.chance(0.2) {
            continue;
        }
        let token = format!("value-{iteration}-{source}-{key}");
        let name = format!("k{key}");
        let (name, text, value) = match random.index(7) {
            0 => (name, format!("\"{token}\""), Value::String(token)),
            1 => (name, format!("[shared, \"{token}\"]"), Value::Seq(vec![
                Value::String("shared".into()), Value::String(token),
            ])),
            2 => {
                let number = (source + iteration * 4).to_string();
                (name, number.clone(), Value::Atom(number))
            }
            3 => (name, "[]".into(), Value::Seq(Vec::new())),
            4 => (name, format!("\"shared\\n{token} --> end\""),
                Value::String(format!("shared\n{token} --> end"))),
            5 => (name, format!("\"한글-{token} 🙂\""),
                Value::String(format!("한글-{token} 🙂"))),
            6 => (format!("키{key}"), format!("\"{token}\""), Value::String(token)),
            _ => unreachable!("seven choices"),
        };
        fields.push(format!("{name}: {text}"));
        keys.push((name, value));
    }
    // Duplicate keys invalidate the entire YAML header upstream: its raw text is body.
    if random.chance(0.06) && !fields.is_empty() {
        fields.push(fields[0].clone());
        keys.clear();
    }
    let eol = if random.chance(0.2) { "\r\n" } else { "\n" };
    let content = if fields.is_empty() {
        keys.push(("empty".into(), Value::String("shared".into())));
        "empty: shared".into()
    } else {
        fields.join(eol)
    };
    let closing = if random.chance(0.1) { "---".into() } else { format!("---{eol}") };
    Header { text: format!("---{eol}{content}{eol}{closing}"), keys }
}

#[test]
fn seeded_mixed_kind_four_source_adoption_preserves_values_and_replays() {
    for seed_value in [20261016, 0x5a17c0de] {
        let mut random = Random::new(seed_value);
        for iteration in 0..2000 {
            // Given: the same four-source generator, draw order and seeds as upstream A1.
            let sources: Vec<_> = (0..4).map(|n| header(&mut random, n, iteration)).collect();
            let files = [
                Snapshot { path: "n.md".into(), text: format!("{}{}", sources[1].text, block(&sources[2].text)) },
                Snapshot { path: "n.sync-conflict-20261015-device.md".into(), text: sources[3].text.clone() },
            ];
            let initial = ItemState {
                current_text: Some(sources[0].text.clone()), last_materialised: None,
                tombstoned: false, registers: sole_registers(&sources[0].text),
            };
            // When: adopt T2 and T1 sources into the pre-existing space.
            let plan = plan_adoption(&files, &initial).expect("plan");
            // Then: every independently generated valid key/value survives, regardless of kind.
            let context = format!("A1 seed={seed_value} case={iteration}");
            let facts = Facts::read(&plan.rendered);
            for source in &sources {
                facts.assert_keys(&source.keys, &context);
            }
            assert!(!plan.rendered.contains("memory-sync frontmatter conflict"), "{context}");
            assert!(!matches!(plan.action, AdoptionAction::Remove), "{context}");
            let repeated = plan_adoption(&files, &ItemState {
                current_text: Some(plan.rendered.clone()), last_materialised: None,
                tombstoned: false, registers: plan.registers.clone(),
            }).expect("repeat");
            assert!(matches!(repeated.action, AdoptionAction::None), "{context}");
            if iteration % 25 == 0 {
                let doc = seed(&sources[0].text);
                let committed = plan_adoption(&files, &state(&doc)).expect("committed plan");
                if matches!(committed.action, AdoptionAction::Capture { .. }) {
                    commit(&doc, &committed);
                }
                assert_eq!(render_doc_content(&doc).expect("render"), committed.rendered, "{context}");
                let mut expected = committed.registers;
                for values in expected.values_mut() {
                    values.sort();
                }
                assert_eq!(item::active_registers(&doc), expected, "{context}");
            }
        }
    }
}

#[test]
fn duplicate_key_headers_remain_body_and_the_side_is_removable_only_with_its_lines() {
    // Given: invalid duplicate YAML keys must not collapse into a single adopted field.
    let doc = seed("base\n");
    let files = [Snapshot {
        path: "n.sync-conflict-20261015-mac.md".into(),
        text: "---\nk: first\nk: second\n---\ntail\n".into(),
    }];
    // When
    let plan = plan_adoption(&files, &state(&doc)).expect("plan");
    commit(&doc, &plan);
    // Then: both raw key lines survive, without inventing a register for invalid YAML.
    let output = render_doc_content(&doc).expect("render");
    assert!(output.lines().any(|line| line == "k: first"));
    assert!(output.lines().any(|line| line == "k: second"));
    assert!(!item::active_registers(&doc).contains_key("k"));
    assert_eq!(confirm_removals(&doc, &plan, &files).expect("gate").remove, [files[0].path.clone()]);
}

#[test]
fn existing_key_hub_and_side_values_survive_capture_into_real_item() {
    // Given
    let doc = seed("");
    let main = "---\ntitle: Main\ntags: [a]\nreviewed: true\n---\nmain\n";
    let other = "---\ntitle: Other\ntags: [a, b]\nreviewed: false\n---\n";
    let side = "---\ntitle: Side\ntags: [c]\nreviewed: 3\n---\nside\n";
    let files = [
        Snapshot { path: "n.md".into(), text: format!("{main}{}", block(other)) },
        Snapshot { path: "n.sync-conflict-20261015-device.md".into(), text: side.into() },
    ];
    // When
    let plan = plan_adoption(&files, &state(&doc)).expect("plan");
    commit(&doc, &plan);
    // Then
    let output = render_doc_content(&doc).expect("render");
    assert_eq!(output, plan.rendered);
    let facts = Facts::read(&output);
    for value in ["Main", "Other", "Side"] {
        assert!(facts.holds("title", &Value::String(value.into())));
    }
    for value in ["true", "false", "3"] {
        assert!(facts.holds("reviewed", &Value::Atom(value.into())));
    }
    for value in ["a", "b", "c"] {
        assert!(facts.holds("tags", &Value::String(value.into())));
    }
    let body = crate::sync::item::frontmatter::split_item(&output).body;
    assert_eq!(body, "main\nside\n");
}
