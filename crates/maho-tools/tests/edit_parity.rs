use maho_tools::{definition::*, edit::{create_edit_tool_definition, EditToolOptions}, edit_diff::*};
use serde_json::{Value,json};

#[test]
fn upstream_edit_function_goldens() {
    let fixtures: Value = serde_json::from_str(include_str!("golden/tools-edit.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let args = fixture["args"].as_array().unwrap();
        let result = match fixture["export"].as_str().unwrap() {
            "generateDiffString" => serde_json::to_value(generate_diff_string(args[0].as_str().unwrap(),args[1].as_str().unwrap(),4)).unwrap(),
            "generateUnifiedPatch" => json!(generate_unified_patch(args[0].as_str().unwrap(),args[1].as_str().unwrap(),args[2].as_str().unwrap(),4)),
            "applyEditsToNormalizedContent" => {
                let edits: Vec<Edit> = serde_json::from_value(args[1].clone()).unwrap();
                let result = apply_edits_to_normalized_content(args[0].as_str().unwrap(),&edits,args[2].as_str().unwrap()).unwrap();
                json!({"baseContent":result.base_content,"newContent":result.new_content})
            }
            "normalizeForFuzzyMatch" => json!(normalize_for_fuzzy_match(args[0].as_str().unwrap())),
            other => panic!("unexpected fixture {other}"),
        };
        assert_eq!(result,fixture["result"],"{}",fixture["export"]);
    }
}

#[tokio::test]
async fn multi_hunk_edit_matches_upstream_diff() {
    let temp = tempfile::tempdir().unwrap();
    let fixtures: Value = serde_json::from_str(include_str!("golden/tools-edit.json")).unwrap();
    let before = fixtures[0]["args"][0].as_str().unwrap();
    tokio::fs::write(temp.path().join("sample.txt"),before).await.unwrap();
    let tool = create_edit_tool_definition(temp.path().into(),EditToolOptions::default());
    let result = (tool.execute)(ToolCall { id:"edit-qa", params:json!({"path":"sample.txt","edits":[{"oldText":"one\n","newText":"ONE\n"},{"oldText":"twelve\n","newText":"TWELVE\n"}]}),signal:AbortSignal::default(),on_update:None,context:None }).await.unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["diff"],fixtures[0]["result"]["diff"]);
    assert_eq!(details["patch"],fixtures[1]["result"]);
    assert_eq!(tokio::fs::read_to_string(temp.path().join("sample.txt")).await.unwrap(),fixtures[0]["args"][1].as_str().unwrap());
    println!("{}",details["diff"].as_str().unwrap());
}

#[test]
fn original_offsets_and_overlaps_are_enforced() {
    let result = apply_edits_to_normalized_content("first second", &[Edit { old_text:"first".into(),new_text:"second".into() },Edit { old_text:"second".into(),new_text:"third".into() }],"a").unwrap();
    assert_eq!(result.new_content,"second third");
    let error = apply_edits_to_normalized_content("abcdef", &[Edit { old_text:"abc".into(),new_text:"x".into() },Edit { old_text:"bc".into(),new_text:"y".into() }],"a").unwrap_err();
    assert!(error.to_string().contains("overlap"));
}
