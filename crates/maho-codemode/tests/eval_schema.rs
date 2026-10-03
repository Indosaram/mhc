use maho_codemode::{config::settings::Languages,tool::types::*};

#[test]
fn schema_preserves_language_order_and_control_requirements() {
    let schema=create_eval_input_schema(&Languages{js:true,py:true,rb:true,jl:true},&EvalDeadlineSeconds::default()).unwrap();
    let values:Vec<_>=schema["properties"]["language"]["anyOf"].as_array().unwrap().iter().map(|item|item["const"].as_str().unwrap()).collect();
    assert_eq!(values,vec!["js","py","rb","jl"]);
    assert_eq!(schema["anyOf"][1]["required"],serde_json::json!(["action","cell_id"]));
    assert_eq!(schema["properties"]["timeout"]["minimum"],1);
}

#[test]
fn schema_filters_disabled_languages_and_rejects_empty_selection() {
    let mut languages=Languages{js:false,py:true,rb:false,jl:false};
    assert_eq!(create_eval_input_schema(&languages,&EvalDeadlineSeconds::default()).unwrap()["properties"]["language"]["anyOf"].as_array().unwrap().len(),1);
    languages.py=false;
    assert!(create_eval_input_schema(&languages,&EvalDeadlineSeconds::default()).is_err());
}
