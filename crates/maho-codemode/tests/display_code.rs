use maho_codemode::tool::{display_code::display_code,types::EvalLanguage};

#[test]
fn dense_code_breaks_blocks_and_arrays_without_changing_tokens() {
    let code=r#"for(let i=0;i<4;i++){print("=== "+i+" ===");print(groupingWave[i].text.split("\n\nAdditional")[0]);} const d=await parallel([()=>tool.grep({pattern:"goal-cache|rule-activation",path:"/repo/apps",limit:80}),()=>tool.read({path:"/repo/notes.md"})]);for(let i=0;i<d.length;i++){print(d[i].text)}"#;
    let output=display_code(code,EvalLanguage::Js);
    assert!(output.lines().any(|line|line=="  print(d[i].text)"));
    assert!(output.lines().any(|line|line=="const d=await parallel(["));
    let tokens=|source:&str|source.chars().filter(|character|!character.is_whitespace()).collect::<String>();
    assert_eq!(tokens(&output),tokens(code));
}

#[test]
fn comments_are_preserved_between_statements() {
    let code=r#"const first=await tool.read({path:"/repo/a.md"}); /* then the second file */ const second=await tool.read({path:"/repo/b.md"});"#;
    let output=display_code(code,EvalLanguage::Js);
    assert_eq!(output.lines().count(),3);
    assert_eq!(output.lines().nth(1),Some("/* then the second file */"));
}

#[test]
fn readable_other_language_and_malformed_code_are_unchanged() {
    for (code,language) in [("const x=1;\nprint(x);".into(),EvalLanguage::Js),(format!("const x=({}","a + ".repeat(40)),EvalLanguage::Js),(format!("print({})","1 + ".repeat(40)),EvalLanguage::Py)] {
        assert_eq!(display_code(&code,language),code);
    }
}
