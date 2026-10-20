use super::corpus_support::{Facts, Random, block, hand_block};
use super::tests::{commit, seed, state};
use super::*;
use crate::sync::item::{frontmatter::split_item, render::render_doc_content, yaml::Value};

struct Built {
    text: String,
    lines: Vec<String>,
    keys: Vec<(String, Value)>,
    key_lines: Vec<(String, String, Value)>,
    malformed: bool,
}

fn build(random: &mut Random, tag: &str) -> Built {
    let pool = [
        "- likes tea", "- likes coffee", "abc", "ax", "note: alpha",
        "prefers 한글", "emoji 🙂", "-->", "--- not fm",
    ];
    let mut built = Built {
        text: String::new(), lines: Vec::new(), keys: Vec::new(),
        key_lines: Vec::new(), malformed: false,
    };
    if random.chance(0.4) {
        built.keys.push(("shared".into(), Value::String(format!("v {tag}"))));
        built.keys.push(("tags".into(), Value::Seq(vec![
            Value::String("shared".into()), Value::String(tag.into()),
        ])));
        built.text.push_str(&format!("---\nshared: \"v {tag}\"\ntags: [shared, \"{tag}\"]\n---\n"));
    }
    let count = 1 + random.index(5);
    for i in 0..count {
        let line = pool[random.index(pool.len())];
        built.lines.push(line.into());
        built.text.push_str(&format!("{line}\n"));
        // One draw selects all three intervals, as in upstream build().
        let roll = random.unit();
        let key = format!("k_{tag}_{i}");
        let value = format!("v {tag} {i}");
        let marker = if roll < 0.3 {
            let block_key = if random.chance(0.3) { "shared" } else { &key };
            built.keys.push((block_key.into(), Value::String(value.clone())));
            block(&format!("---\n{block_key}: \"{value}\"\n---\n"))
        } else if roll < 0.45 {
            built.malformed = true;
            let key_line = format!("{key}: \"{value}\"");
            built.key_lines.push((key, key_line.clone(), Value::String(value)));
            let inner = if random.chance(0.5) {
                vec![format!("inside {tag} {i}")]
            } else {
                vec![format!("FACT {tag} {i} -->"), format!("more {tag} {i}")]
            };
            built.lines.extend(inner.iter().cloned());
            hand_block(&format!("---\n{key_line}\n---\n{}\n", inner.join("\n")))
        } else {
            String::new()
        };
        if !marker.is_empty() {
            built.text.push_str(if random.chance(0.5) { &marker[1..] } else { &marker });
        }
    }
    if random.chance(0.25) {
        built.text = built.text.replace('\n', "\r\n");
    }
    built
}

#[test]
fn seeded_multisource_readback_preserves_every_fact_of_each_removed_source() {
    for seed_value in [20261016, 0x5eed4a11] {
        let mut random = Random::new(seed_value);
        for iteration in 0..1500 {
            // Given: known facts, one main and one or two sides, including hand-edited blocks.
            let main = build(&mut random, &format!("m{iteration}"));
            let count = 1 + random.index(2);
            let sides: Vec<_> = (0..count).map(|n| {
                let label = if n == 0 { "mac" } else { "mini" };
                (format!("n.sync-conflict-20261015-{label}.md"),
                    build(&mut random, &format!("s{iteration}-{n}")))
            }).collect();
            let lossy = if random.chance(0.3) { Some(random.index(2)) } else { None };
            let files: Vec<_> = std::iter::once(Snapshot { path: "n.md".into(), text: main.text.clone() })
                .chain(sides.iter().map(|(path, built)| Snapshot { path: path.clone(), text: built.text.clone() }))
                .collect();
            let doc = seed("base line\n");
            // When: commit either faithful alignment, line deduplication, or a dropped first line.
            let plan = plan_adoption_with_merge(&files, &state(&doc), |base, incoming| match lossy {
                None => merge_insertions(base, incoming),
                Some(0) => {
                    let joined = format!("{base}{incoming}");
                    let mut seen = std::collections::BTreeSet::new();
                    read_back::lines_of(&joined).into_iter()
                        .filter(|line| seen.insert(*line))
                        .map(|line| format!("{line}\n")).collect()
                }
                Some(1) => format!("{base}{}", read_back::lines_of(incoming).into_iter()
                    .skip(1).map(|line| format!("{line}\n")).collect::<String>()),
                Some(_) => unreachable!("two lossy merges"),
            }).expect("plan");
            if matches!(plan.action, AdoptionAction::Capture { .. }) {
                commit(&doc, &plan);
            }
            let output = render_doc_content(&doc).expect("render");
            let decision = confirm_removals(&doc, &plan, &files).expect("gate");
            // Then: independently recorded facts, not reparsed source text, authorize removal.
            let context = format!("R1 seed={seed_value} case={iteration} output={output:?}");
            let body = split_item(&output).body;
            let lines = read_back::lines_of(&body);
            let facts = Facts::read(&output);
            assert!(lines.iter().all(|line| !line.starts_with("<!-- memory-sync frontmatter conflict")), "{context}");
            facts.assert_keys(&main.keys, &context);
            if lossy.is_none() {
                for line in &main.lines {
                    assert!(lines.contains(&line.as_str()), "{context}: main lost {line}");
                }
                if !main.malformed && sides.iter().all(|(_, built)| !built.malformed) {
                    assert!(lines.iter().all(|line| !line.contains("kept here so no fact is lost")), "{context}");
                }
            }
            for (path, built) in &sides {
                if !decision.remove.contains(path) {
                    assert!(decision.retained.iter().any(|entry| &entry.path == path && !entry.reason.is_empty()), "{context}: unaccounted {path}");
                    continue;
                }
                assert!(!decision.retained.iter().any(|entry| &entry.path == path), "{context}");
                for line in &built.lines {
                    let wanted = built.lines.iter().filter(|candidate| *candidate == line).count();
                    let held = lines.iter().filter(|candidate| **candidate == line.as_str()).count();
                    assert!(held >= wanted, "{context}: {path} lost {line:?} ({held}/{wanted})");
                }
                facts.assert_keys(&built.keys, &context);
                for (key, line, value) in &built.key_lines {
                    assert!(lines.contains(&line.as_str()) || facts.holds(key, value), "{context}: {path} lost {line}");
                }
            }
        }
    }
}
