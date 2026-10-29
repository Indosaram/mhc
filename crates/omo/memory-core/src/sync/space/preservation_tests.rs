use super::tests::{commit, seed, state};
use super::*;
use crate::sync::item::{
    self,
    frontmatter::{FmValue, split_item},
    render::render_doc_content,
};

#[test]
fn whole_input_lines_survive_alignment_and_fallback_for_deterministic_fixture_product() {
    // Given: shared prose, duplicate lines, Unicode, CRLF and unterminated inputs.
    let pool = [
        "Prefers dark mode in every editor.",
        "Prefers short commit messages.",
        "",
        "한글 줄 그대로",
        "emoji 🙂🚀",
        "a",
        "a",
    ];
    let mut texts = vec![String::new()];
    for a in pool {
        for b in pool {
            for eol in ["\n", "\r\n"] {
                for terminated in [false, true] {
                    texts.push(format!("{a}{eol}{b}{}", if terminated { eol } else { "" }));
                }
            }
        }
    }
    for a in &texts {
        for b in &texts {
            for budget in [1, merge::DEFAULT_MAX_CELLS] {
                // When
                let output = merge_insertions_with_budget(a, b, budget);
                // Then
                let actual = read_back::lines_of(&output);
                let inputs: Vec<_> = read_back::lines_of(a)
                    .into_iter()
                    .chain(read_back::lines_of(b))
                    .collect();
                assert!(
                    inputs.iter().all(|line| actual.contains(line)),
                    "{a:?} {b:?} {output:?}"
                );
                assert!(actual.iter().all(|line| inputs.contains(line)));
                assert!(is_ordered_subsequence(a, &output));
                assert!(is_ordered_subsequence(b, &output));
            }
        }
    }
}

#[test]
fn four_source_adoption_preserves_whole_values_and_replays_idempotently() {
    // Given: four independently known register facts and shared set elements.
    for iteration in 0..2000 {
        let eol = if iteration % 5 == 0 { "\r\n" } else { "\n" };
        let make = |source| {
            format!(
                "---{eol}title: v-{iteration}-{source}{eol}tags: [shared, s{source}]{eol}---{eol}"
            )
        };
        let base = make(0);
        let main = make(1);
        let other = make(2);
        let side = make(3);
        let doc = seed(&base);
        let sources = vec![
            Snapshot {
                path: "n.md".into(),
                text: format!(
                    "{main}\n<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:\n{}-->\n",
                    other.replace("\r\n", "\n")
                ),
            },
            Snapshot {
                path: "n.sync-conflict-20261015-mac.md".into(),
                text: side,
            },
        ];
        // When
        let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
        commit(&doc, &plan);
        // Then: assertions use the fixture's known facts, not a reparse of the source.
        let mut expected: Vec<_> = (0..4)
            .map(|source| format!("v-{iteration}-{source}"))
            .collect();
        expected.sort();
        assert_eq!(item::active_registers(&doc)["title"], expected);
        let rendered = render_doc_content(&doc).expect("render");
        assert_eq!(rendered, plan.rendered);
        let tags = split_item(&rendered)
            .fm
            .expect("frontmatter")
            .into_iter()
            .find(|entry| entry.key == "tags")
            .expect("tags");
        assert!(
            matches!(tags.value, FmValue::Set { elements, .. } if elements == ["shared", "s0", "s1", "s2", "s3"])
        );
        assert_eq!(
            confirm_removals(&doc, &plan, &sources)
                .expect("gate")
                .remove,
            [sources[1].path.clone()]
        );
        let replay = plan_adoption(&sources, &state(&doc)).expect("replay");
        assert!(matches!(replay.action, AdoptionAction::None));
    }
}

#[test]
fn committed_deletion_gate_preserves_known_facts_or_retains_source_with_reason() {
    // Given: independent known facts with duplicates, CRLF, valid and malformed blocks.
    for iteration in 0..2000 {
        let value = format!("v-{iteration}");
        let extra = format!("inside-{iteration}");
        let malformed = iteration % 3 == 0;
        let marker = format!(
            "<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:\n---\nk: {value}\n---\n{}-->\n",
            if malformed {
                format!("{extra}\n")
            } else {
                String::new()
            }
        );
        let raw = format!(
            "---\nshared: {value}\ntags: [shared, t{iteration}]\n---\nx\nd\nd\n{marker}tail\n"
        );
        let text = if iteration % 4 == 0 {
            raw.replace('\n', "\r\n")
        } else {
            raw
        };
        let sources = vec![Snapshot {
            path: "n.sync-conflict-20261015-mac.md".into(),
            text,
        }];
        let doc = seed("base\n");
        // When: either a faithful or deliberately lossy merge is committed.
        let plan = plan_adoption_with_merge(&sources, &state(&doc), |base, incoming| {
            if iteration % 5 == 0 {
                let merged = merge_insertions(base, incoming);
                let mut seen = std::collections::BTreeSet::new();
                read_back::lines_of(&merged)
                    .into_iter()
                    .filter(|line| seen.insert(*line))
                    .map(|line| format!("{line}\n"))
                    .collect()
            } else {
                merge_insertions(base, incoming)
            }
        })
        .expect("plan");
        if matches!(&plan.action, AdoptionAction::Capture { .. }) {
            commit(&doc, &plan);
        }
        let decision = confirm_removals(&doc, &plan, &sources).expect("gate");
        // Then: each source is exclusively removable or retained with a reason.
        if decision.remove.contains(&sources[0].path) {
            assert!(decision.retained.is_empty());
            let rendered = render_doc_content(&doc).expect("render");
            let body = split_item(&rendered).body;
            let lines = read_back::lines_of(&body);
            assert_eq!(lines.iter().filter(|line| **line == "d").count(), 2);
            for line in ["x", "tail"] {
                assert!(lines.contains(&line));
            }
            if malformed {
                assert!(lines.contains(&extra.as_str()));
            } else {
                assert_eq!(item::active_registers(&doc)["k"], std::slice::from_ref(&value));
            }
            assert_eq!(item::active_registers(&doc)["shared"], [value]);
            let tags = split_item(&rendered)
                .fm
                .expect("frontmatter")
                .into_iter()
                .find(|entry| entry.key == "tags")
                .expect("tags");
            assert!(
                matches!(tags.value, FmValue::Set { elements, .. } if elements.contains(&"shared".to_owned()) && elements.contains(&format!("t{iteration}")))
            );
        } else {
            assert_eq!(decision.retained.len(), 1);
            assert_eq!(decision.retained[0].path, sources[0].path);
            assert!(!decision.retained[0].reason.is_empty());
        }
    }
}

#[test]
fn bom_main_and_side_materialize_with_original_bom_but_no_bom_in_item() {
    // Given
    let doc = seed("---\ntitle: A\n---\nbody\n");
    let main = "\u{feff}---\ntitle: A\n---\nbody\n";
    let sources = vec![
        Snapshot {
            path: "n.md".into(),
            text: main.into(),
        },
        Snapshot {
            path: "n.sync-conflict-20261015-mac.md".into(),
            text: "\u{feff}---\ntitle: B\n---\nbody\n".into(),
        },
    ];
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    commit(&doc, &plan);
    // Then
    assert_eq!(
        item::render_for_disk(&doc, main)
            .expect("disk render")
            .as_deref(),
        Some("\u{feff}---\ntitle: \"A / B\"\n---\nbody\n")
    );
    assert!(
        !render_doc_content(&doc)
            .expect("render")
            .contains('\u{feff}')
    );
    assert_eq!(
        confirm_removals(&doc, &plan, &sources)
            .expect("gate")
            .remove,
        [sources[1].path.clone()]
    );
}
