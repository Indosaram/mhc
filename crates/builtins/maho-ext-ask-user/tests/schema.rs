use maho_ext_ask_user::schema::*;
use serde_json::json;
#[test]
fn missing_wait_rejected(){for variant in [AskUserVariant::Codex,AskUserVariant::Claude]{assert!(to_canonical(variant,&json!({"questions":[]}),"r".into(),None).is_err());}}
#[test]
fn claude_generated_ids_and_optional_options(){let question=json!({"header":" Auth ","question":" Pick? ","multiSelect":false});let request=to_canonical(AskUserVariant::Claude,&json!({"waitForAnswer":false,"questions":[question,question]}),"r".into(),None).unwrap();assert_eq!(request.questions.iter().map(|question|question.id.as_str()).collect::<Vec<_>>(),["q1","q2"]);assert!(request.questions[0].options.is_empty());assert_eq!(request.questions[0].header,"Auth");assert_eq!(request.timeout_ms,1_800_000);}
#[test]
fn codex_ids_options_and_multiselect(){let mut args=json!({"wait_for_answer":true,"questions":[{"id":"auth_2","header":"Auth","question":"Pick?","options":[{"label":" A ","description":" Alpha "},{"label":"B","description":"Beta"}]}]});let request=to_canonical(AskUserVariant::Codex,&args,"r".into(),Some(7)).unwrap();assert_eq!(request.questions[0].options[0].label,"A");assert!(!request.questions[0].multi_select);assert_eq!(request.timeout_ms,7);for id in ["Auth","_auth","auth__2","auth_"]{args["questions"][0]["id"]=json!(id);assert!(to_canonical(AskUserVariant::Codex,&args,"r".into(),None).is_err());}}
#[test]
fn claude_bounds(){let question=json!({"header":"Auth","question":"Pick?","multiSelect":false});for count in [0,5]{assert!(to_canonical(AskUserVariant::Claude,&json!({"waitForAnswer":true,"questions":vec![question.clone();count]}),"r".into(),None).is_err());}let mut args=json!({"waitForAnswer":true,"questions":[question]});args["questions"][0]["header"]=json!("😀😀😀😀😀😀a");assert!(to_canonical(AskUserVariant::Claude,&args,"r".into(),None).is_err());}
#[test]
fn option_bounds_and_empty_headers_follow_upstream(){
    let question=json!({"header":"Auth","question":"Pick?","multiSelect":false});
    for count in [1,5]{
        let mut q=question.clone();q["options"]=json!(vec![json!({"label":"A","description":"Choice"});count]);
        assert!(to_canonical(AskUserVariant::Claude,&json!({"waitForAnswer":true,"questions":[q]}),"r".into(),None).is_err());
    }
    let mut q=question;q["header"]=json!("");
    assert!(to_canonical(AskUserVariant::Claude,&json!({"waitForAnswer":true,"questions":[q]}),"r".into(),None).is_err());
}
