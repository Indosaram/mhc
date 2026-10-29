use super::*;

#[test]
fn aliases_are_add_only_when_repeated() {
    // Given
    let doc = create_manifest_doc(1).expect("manifest");
    manifest_add_binding(&doc, "repo:github.com/a/b").expect("binding");
    manifest_add_binding(&doc, "named:Dori").expect("binding");
    let before = doc.oplog_vv();
    // When
    let changed = manifest_add_binding(&doc, "named:Dori").expect("binding");
    // Then
    assert!(!changed);
    assert_eq!(doc.oplog_vv(), before);
    assert_eq!(
        manifest_bindings(&doc),
        ["named:Dori", "repo:github.com/a/b"]
    );
    assert!(manifest_has_binding(&doc, "named:Dori"));
    assert!(!manifest_has_binding(&doc, "named:missing"));
}

#[test]
fn whole_manifest_survives_update_import() {
    // Given
    let a = create_manifest_doc(1).expect("manifest");
    let b = create_manifest_doc(2).expect("manifest");
    manifest_add_binding(&a, "named:Dori").expect("binding");
    manifest_set_display_name(&a, "alpha").expect("name");
    manifest_mark_absorbed(&a, "aaaa").expect("tombstone");
    // When
    b.import(&manifest_export(&a).expect("export"))
        .expect("import");
    // Then
    assert_eq!(manifest_bindings(&b), ["named:Dori"]);
    assert_eq!(manifest_display_name(&b).as_deref(), Some("alpha"));
    assert_eq!(manifest_absorbed_into(&b).as_deref(), Some("aaaa"));
}

#[test]
fn concurrent_aliases_and_names_converge() {
    // Given
    let a = create_manifest_doc(1).expect("manifest");
    let b = create_manifest_doc(2).expect("manifest");
    manifest_add_binding(&a, "repo:github.com/a/b").expect("binding");
    manifest_add_binding(&b, "named:Dori").expect("binding");
    manifest_set_display_name(&a, "alpha").expect("name");
    manifest_set_display_name(&b, "beta").expect("name");
    let a_update = manifest_export(&a).expect("export");
    let b_update = manifest_export(&b).expect("export");
    // When
    a.import(&b_update).expect("import");
    b.import(&a_update).expect("import");
    // Then
    assert_eq!(manifest_bindings(&a), ["named:Dori", "repo:github.com/a/b"]);
    assert_eq!(manifest_bindings(&a), manifest_bindings(&b));
    assert!(manifest_display_name(&a).is_some());
    assert_eq!(manifest_display_name(&a), manifest_display_name(&b));
}

#[test]
fn absorption_keeps_first_value_when_replayed() {
    // Given
    let doc = create_manifest_doc(1).expect("manifest");
    manifest_mark_absorbed(&doc, "aaaa").expect("tombstone");
    let before = doc.oplog_vv();
    // When
    manifest_mark_absorbed(&doc, "bbbb").expect("tombstone");
    // Then
    assert_eq!(manifest_absorbed_into(&doc).as_deref(), Some("aaaa"));
    assert_eq!(doc.oplog_vv(), before);
}

#[test]
fn display_name_does_not_record_identical_write() {
    // Given
    let doc = create_manifest_doc(1).expect("manifest");
    assert_eq!(manifest_display_name(&doc), None);
    manifest_set_display_name(&doc, "alpha").expect("name");
    let before = doc.oplog_vv();
    // When
    manifest_set_display_name(&doc, "alpha").expect("name");
    // Then
    assert_eq!(doc.oplog_vv(), before);
}

#[test]
fn join_preserves_aliases_and_is_idempotent() {
    // Given
    let a = create_manifest_doc(1).expect("manifest");
    let b = create_manifest_doc(2).expect("manifest");
    manifest_add_binding(&a, "named:shared").expect("binding");
    manifest_add_binding(&b, "named:shared").expect("binding");
    manifest_add_binding(&b, "named:alias").expect("binding");
    // When
    let result = join_spaces(&b, "bbbb", &a, "aaaa").expect("join");
    // Then
    assert_eq!(
        result,
        JoinResult {
            target_id: "aaaa".into(),
            absorbed_id: "bbbb".into(),
            changed: true
        }
    );
    assert_eq!(manifest_bindings(&a), ["named:alias", "named:shared"]);
    assert_eq!(manifest_absorbed_into(&b).as_deref(), Some("aaaa"));
    assert!(!join_spaces(&a, "aaaa", &b, "bbbb").expect("replay").changed);
}
