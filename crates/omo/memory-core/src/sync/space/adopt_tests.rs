use super::tests::{commit, files, seed, state};
use super::*;
use crate::sync::item::{self, frontmatter::{FmValue, split_item}, render::render_doc_content};

#[test]
fn set_adoption_unions_elements_without_changing_key_kind() {
    // Given
    let doc = seed("---\ntags: [a, b]\n---\nx\n");
    let sources = files("---\ntags: [b, c]\n---\nx\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    commit(&doc, &plan);
    // Then
    let parsed = split_item(&render_doc_content(&doc).expect("render"));
    let entries = parsed.fm.expect("header");
    assert!(matches!(&entries[0].value, FmValue::Set { elements, .. } if elements == &vec!["a", "b", "c"]));
    assert_eq!(confirm_removals(&doc, &plan, &sources).expect("gate").remove, vec![sources[0].path.clone()]);
}

#[test]
fn cross_kind_values_are_adopted_as_whole_register_or_set_elements() {
    // Given
    let doc = seed("---\nreg: old\ntags: [a]\n---\nx\n");
    let sources = files("---\nreg: [b, c]\ntags: other\n---\nx\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    commit(&doc, &plan);
    // Then
    let registers = item::active_registers(&doc);
    assert_eq!(registers["reg"], vec!["[ b, c ]", "old"]);
    let parsed = split_item(&render_doc_content(&doc).expect("render"));
    let tags = parsed.fm.expect("header").into_iter().find(|e| e.key == "tags").expect("tags");
    assert!(matches!(tags.value, FmValue::Set { elements, .. } if elements == vec!["a", "other"]));
}

#[test]
fn scalar_types_and_mapping_values_remain_distinct_register_entries() {
    // Given
    let doc = seed("---\nn: 3\nm: {a: 1}\n---\nx\n");
    let sources = files("---\nn: '3'\nm: {a: 2}\n---\nx\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    commit(&doc, &plan);
    // Then
    let registers = item::active_registers(&doc);
    assert_eq!(registers["n"], vec!["\"3\"", "3"]);
    assert_eq!(registers["m"], vec!["{ a: 1 }", "{ a: 2 }"]);
}

#[test]
fn hub_frontmatter_is_captured_with_main_and_side_values() {
    // Given
    let doc = seed("");
    let sources = vec![
        Snapshot { path: "n.md".into(), text: "---\ntitle: Main\n---\nmain\n\n<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:\n---\ntitle: Other\n---\n-->\n".into() },
        Snapshot { path: "n.sync-conflict-20261015-mac.md".into(), text: "---\ntitle: Side\n---\nmain\n".into() },
    ];
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    commit(&doc, &plan);
    // Then
    assert_eq!(item::active_registers(&doc)["title"], vec!["Main", "Other", "Side"]);
    assert_eq!(split_item(&render_doc_content(&doc).expect("render")).body, "main\n");
}

#[test]
fn changed_file_over_tombstone_is_adopted_as_concurrent_edit() {
    // Given
    let doc = seed("body\n");
    item::delete_item(&doc).expect("delete");
    let mut item = state(&doc);
    item.tombstoned = true;
    item.last_materialised = Some("body\n".into());
    let sources = vec![Snapshot { path: "n.md".into(), text: "body\nchanged\n".into() }];
    let plan = plan_adoption(&sources, &item).expect("plan");
    // When
    commit(&doc, &plan);
    // Then
    assert!(item::is_live(&doc));
    assert_eq!(render_doc_content(&doc).expect("render"), "body\nchanged\n");
}

#[test]
fn missing_adopted_source_never_qualifies_for_removal() {
    // Given
    let doc = seed("body\n");
    let sources = files("body\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    // When
    let decision = confirm_removals(&doc, &plan, &[]).expect("gate");
    // Then
    assert!(decision.remove.is_empty());
    assert_eq!(decision.retained, vec![RetainedSource { path: sources[0].path.clone(), reason: "source not among the adopted files".into() }]);
}
