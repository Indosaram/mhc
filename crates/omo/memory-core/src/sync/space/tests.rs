use super::*;
use crate::sync::item::{
    self, adopt_values::with_register_values, capture::capture_item, document::item_frontier,
    render::render_doc_content,
};
use std::collections::BTreeMap;

impl SpaceConfigStore for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Result<Option<String>, SpaceError> {
        Ok(BTreeMap::get(self, key).cloned())
    }
    fn set(&mut self, key: &str, value: &str) -> Result<(), SpaceError> {
        self.insert(key.into(), value.into());
        Ok(())
    }
}
pub(super) fn state(doc: &loro::LoroDoc) -> ItemState {
    ItemState {
        current_text: Some(render_doc_content(doc).expect("render")),
        tombstoned: false,
        last_materialised: None,
        registers: item::active_registers(doc),
    }
}
pub(super) fn seed(text: &str) -> loro::LoroDoc {
    let doc = item::create_item_doc(1).expect("doc");
    if let Some(update) = capture_item(&doc, "", &vec![], text, None).expect("seed") {
        doc.import(&update).expect("import");
    }
    doc
}
pub(super) fn files(text: &str) -> Vec<Snapshot> {
    vec![Snapshot {
        path: "n.sync-conflict-20261015-mac.md".into(),
        text: text.into(),
    }]
}
pub(super) fn commit(doc: &loro::LoroDoc, plan: &AdoptionPlan) {
    let AdoptionAction::Capture {
        working_text,
        extra_register_values,
    } = &plan.action
    else {
        panic!("capture expected");
    };
    let captured = capture_item(
        doc,
        &render_doc_content(doc).expect("render"),
        &item_frontier(doc),
        working_text,
        None,
    )
    .expect("capture");
    if let Some(update) =
        with_register_values(doc, captured, extra_register_values, None).expect("register values")
    {
        doc.import(&update).expect("import");
    }
}

#[test]
fn canonical_remote_spellings_share_identity() {
    // Given
    let cases = [
        "git@GITHUB.COM:a/b.git",
        "https://user:pass@github.com:443/a/b.GIT/",
        "ssh://git@github.com:22/a/b",
        "git://github.com:9418/a/b",
        "github.com/a/b",
    ];
    // When / Then
    for input in cases {
        assert_eq!(canonical_remote_url(input).expect("url"), "github.com/a/b");
    }
    assert_eq!(
        canonical_remote_url("ssh://git@github.com:2222/A/B").expect("url"),
        "github.com:2222/A/B"
    );
    assert_eq!(
        space_id_for_binding("repo:github.com/a/b"),
        "88c3c57d6699688f3cdd91b31b7e9f4d"
    );
}
#[test]
fn bindings_validate_and_skip_malformed_origin() {
    // Given
    let remotes = vec![
        Remote {
            name: "origin".into(),
            url: "https://[bad".into(),
        },
        Remote {
            name: "upstream".into(),
            url: "git@github.com:a/b.git".into(),
        },
    ];
    // When
    let binding = repo_binding(&remotes);
    // Then
    assert_eq!(binding.as_deref(), Some("repo:github.com/a/b"));
    assert_eq!(named_binding(" A\u{30a} ").expect("profile"), "named:Å");
    assert!(named_binding(" ").is_err());
    assert!(local_binding("AB").is_err());
    assert_eq!(
        parse_binding(&format!("local:{}", "f".repeat(32))).map(|b| b.0),
        Some(BindingKind::Local)
    );
    assert!(parse_binding("local:xyz").is_none());
}
#[test]
fn local_binding_is_minted_only_once() {
    // Given
    let mut store = BTreeMap::new();
    let calls = std::cell::Cell::new(0);
    let random = || {
        calls.set(calls.get() + 1);
        "0".repeat(32)
    };
    let first = resolve_space(
        &mut store,
        ResolveSpaceOptions {
            random_hex: Some(&random),
            ..Default::default()
        },
    )
    .expect("first");
    // When
    let second = resolve_space(
        &mut store,
        ResolveSpaceOptions {
            random_hex: Some(&random),
            ..Default::default()
        },
    )
    .expect("second");
    // Then
    assert_eq!(first.space_id, "d3da1211bd7facd874d5837d6caa380c");
    assert_eq!(first.binding_key, second.binding_key);
    assert_eq!(second.wrote, SpaceWrites::default());
    assert_eq!(calls.get(), 1);
}
#[test]
fn stored_space_survives_remote_rename() {
    // Given
    let mut store = BTreeMap::new();
    let remotes = [Remote {
        name: "origin".into(),
        url: "git@github.com:a/b.git".into(),
    }];
    let first = resolve_space(
        &mut store,
        ResolveSpaceOptions {
            remotes: &remotes,
            ..Default::default()
        },
    )
    .expect("first");
    let renamed = [Remote {
        name: "origin".into(),
        url: "https://github.com/a/renamed.git".into(),
    }];
    // When
    let second = resolve_space(
        &mut store,
        ResolveSpaceOptions {
            remotes: &renamed,
            ..Default::default()
        },
    )
    .expect("renamed");
    // Then
    assert_eq!(second.space_id, first.space_id);
    assert!(second.alias_of_stored_space);
    assert_eq!(second.binding_key, "repo:github.com/a/renamed");
}
#[test]
fn git_config_persists_space_across_resolutions() {
    // Given
    let dir = tempfile::tempdir().expect("directory");
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status()
            .expect("git init")
            .success()
    );
    let first = resolve_space_in_repo(dir.path(), Some("Dori"), None).expect("first");
    // When
    let second = resolve_space_in_repo(dir.path(), Some("renamed"), None).expect("second");
    // Then
    assert_eq!(first.space_id, "852b47fac13bd6a2f0d23f5b5d4fa103");
    assert_eq!(second.space_id, first.space_id);
    assert!(second.alias_of_stored_space);
}
#[test]
fn side_files_and_case_collisions_share_item_key() {
    // Given
    let paths: Vec<String> = [
        "Notes.md",
        "notes.md",
        "Notes.sync-conflict-20261015-mini.md",
        "../escape.md",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    // When
    let groups = group_by_item_key(&paths);
    // Then
    assert_eq!(groups.len(), 1);
    assert_eq!(groups["notes.md"], paths[..3]);
    assert_eq!(
        parse_sync_conflict_name(&paths[2]).expect("side").target,
        "Notes.md"
    );
    assert!(parse_sync_conflict_name("a.sync-conflict-20261015-x.md.bak").is_none());
}
fn marker(header: &str) -> String {
    format!(
        "\n<!-- memory-sync 2026-10-15: the other machine had different frontmatter for n.md; kept here so no fact is lost:\n{header}-->\n"
    )
}
#[test]
fn valid_hub_block_is_folded_but_malformed_block_survives() {
    // Given
    let good = marker("---\nk: a -- > b\n---\n");
    let bad = marker("---\nk: v\n---\nSECRET FACT\n");
    let input = format!("first\n{good}{bad}last\n");
    // When
    let stripped = strip_t2_blocks(&input);
    // Then
    assert_eq!(stripped.body, format!("first\n{bad}last\n"));
    assert_eq!(stripped.malformed, vec![bad]);
    assert_eq!(
        stripped.entries[0].value,
        item::FmValue::Reg {
            value: "a --> b".into()
        }
    );
}
#[test]
fn read_back_counts_duplicates_and_bounds_metadata_allowances() {
    // Given
    let cases = [
        (
            "x\nd\n",
            "x\nd\nd\n",
            "missing line \"d\" (2 in the source, 1 merged)",
        ),
        (
            "x\nsection\n",
            "x\n---\nsection\n---\n",
            "missing line \"---\" (2 dropped, 0 allowed as metadata)",
        ),
        (
            "x\n",
            "x\n-->\n",
            "missing line \"-->\" (1 dropped, 0 allowed as metadata)",
        ),
    ];
    // When / Then
    for (rendered, source, reason) in cases {
        assert_eq!(
            read_back::first_missing_fact(rendered, source, &BTreeMap::new()).as_deref(),
            Some(reason)
        );
    }
}
#[test]
fn joined_register_render_cannot_vouch_for_single_whole_value() {
    // Given
    let text = "---\nk: \"a / b\"\n---\nx\n";
    let registers = BTreeMap::from([("k".into(), vec!["a".into(), "b".into()])]);
    // When
    let missing = read_back::first_missing_fact(text, text, &registers);
    // Then
    assert!(missing.is_some());
}
#[test]
fn adoption_commits_register_values_and_body_before_removal() {
    // Given
    let doc = seed("---\nd: space\n---\nbody\n");
    let sources = files("\u{feff}---\nd: laptop\n---\nbody\nnew line\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    assert!(
        confirm_removals(&doc, &plan, &sources)
            .expect("gate")
            .remove
            .is_empty()
    );
    // When
    commit(&doc, &plan);
    // Then
    assert_eq!(item::active_registers(&doc)["d"], vec!["laptop", "space"]);
    assert_eq!(render_doc_content(&doc).expect("render"), plan.rendered);
    assert_eq!(
        confirm_removals(&doc, &plan, &sources)
            .expect("gate")
            .remove,
        vec![sources[0].path.clone()]
    );
}
#[test]
fn partial_capture_without_extra_register_values_keeps_source() {
    // Given
    let doc = seed("---\nd: space\n---\nbody\n");
    let sources = files("---\nd: laptop\n---\nbody\nnew line\n");
    let plan = plan_adoption(&sources, &state(&doc)).expect("plan");
    let AdoptionAction::Capture { working_text, .. } = &plan.action else {
        panic!("capture");
    };
    // When
    let update = capture_item(
        &doc,
        &render_doc_content(&doc).expect("render"),
        &item_frontier(&doc),
        working_text,
        None,
    )
    .expect("capture")
    .expect("update");
    doc.import(&update).expect("import");
    // Then
    let decision = confirm_removals(&doc, &plan, &sources).expect("gate");
    assert!(decision.remove.is_empty());
    assert_eq!(decision.retained[0].path, sources[0].path);
}
#[test]
fn adopted_sources_replay_without_new_capture() {
    // Given
    let doc = seed("---\nk: base\n---\nbody\n");
    let sources = files("---\nk: other\n---\nmore\n");
    let first = plan_adoption(&sources, &state(&doc)).expect("first");
    commit(&doc, &first);
    // When
    let second = plan_adoption(&sources, &state(&doc)).expect("second");
    // Then
    assert!(matches!(second.action, AdoptionAction::None));
}
#[test]
fn local_deletion_does_not_duplicate_shortened_sentence() {
    // Given
    let doc = seed("a long sentence\n");
    let mut item = state(&doc);
    item.last_materialised = item.current_text.clone();
    let sources = vec![Snapshot {
        path: "n.md".into(),
        text: "a sentence\n".into(),
    }];
    // When
    let plan = plan_adoption(&sources, &item).expect("plan");
    // Then
    assert!(matches!(plan.action, AdoptionAction::None));
    assert_eq!(plan.rendered, "a long sentence\n");
}
#[test]
fn unchanged_materialized_tombstone_removes_instead_of_reviving() {
    // Given
    let item = ItemState {
        current_text: Some("body\n".into()),
        last_materialised: Some("body\n".into()),
        tombstoned: true,
        registers: BTreeMap::new(),
    };
    let sources = vec![Snapshot {
        path: "n.md".into(),
        text: "body\n".into(),
    }];
    // When
    let plan = plan_adoption(&sources, &item).expect("plan");
    // Then
    assert!(matches!(plan.action, AdoptionAction::Remove));
}
#[test]
fn missing_components_and_lossy_merge_fail_closed() {
    // Given
    let mut item = state(&seed("---\nk: base\n---\nbody\n"));
    item.registers.clear();
    // When / Then
    assert!(matches!(
        plan_adoption(&[], &item),
        Err(SpaceError::MissingComponents(_))
    ));
    let doc = seed("body\n");
    let sources = files("body\nunique fact\n");
    let plan =
        plan_adoption_with_merge(&sources, &state(&doc), |base, _| base.into()).expect("plan");
    assert!(plan.removal_candidates.is_empty());
    assert_eq!(plan.retained_sources[0].path, sources[0].path);
}
