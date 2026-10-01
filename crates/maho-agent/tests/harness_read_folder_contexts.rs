use maho_agent::harness::utils::read_folders::*;
const MEMBERS: &str = "\n  alpha: 1,\n  bravo: 2,\n  charlie: 3,\n  delta: 4,\n  echo: 5,\n";
fn fold(text: &str) -> ReadFolderResult {
    SELECTED_READ_FOLDER.fold(ReadFolderInput {
        path: "context.ts",
        text,
        settings: READ_FOLD_SETTINGS,
    })
}
fn value(prefix: &str) {
    let text = format!("{prefix}{{{MEMBERS}}});");
    assert_eq!(
        fold(&text),
        ReadFolderResult::Parsed {
            text,
            ranges: vec![ReadFoldRange {
                start_line: 2,
                end_line: 6,
                children: vec![]
            }]
        }
    );
}
#[test]
fn expression_factory() {
    value("const result = factory(");
}
#[test]
fn expression_member() {
    value("object.factory(");
}
#[test]
fn expression_return() {
    value("return factory(");
}
#[test]
fn expression_await() {
    value("await factory(");
}
#[test]
fn expression_constructor() {
    value("new Factory(");
}
#[test]
fn callback_returned_object() {
    let text = format!("const values = Array.from([], (item) => ({{{MEMBERS}}}));");
    assert_eq!(
        fold(&text),
        ReadFolderResult::Parsed {
            text,
            ranges: vec![ReadFoldRange {
                start_line: 2,
                end_line: 6,
                children: vec![]
            }]
        }
    );
}
fn signature(prefix: &str, suffix: &str) {
    let text = format!("{prefix}{MEMBERS}{suffix}");
    match fold(&text) {
        ReadFolderResult::Parsed { ranges, .. } => {
            let first = prefix.split('\n').count() + 1;
            let mut queue: std::collections::VecDeque<_> = ranges.iter().collect();
            while let Some(r) = queue.pop_front() {
                assert!(!(r.start_line >= first && r.end_line < first + 5));
                queue.extend(&r.children);
            }
        }
        ReadFolderResult::ParseFailure { .. } => {}
        ReadFolderResult::Unsupported { .. } => panic!("unsupported"),
    }
}
#[test]
fn interface_members() {
    signature("interface Factory extends Base {", "}");
}
#[test]
fn type_members() {
    signature("type Factory = {", "};");
}
#[test]
fn callback_type() {
    signature(
        "const values = Array.from([], (item): () => {",
        "} => value);",
    );
}
#[test]
fn constructor_type() {
    signature("type Constructor = new ({", "}) => Value;");
}
#[test]
fn function_binding() {
    signature("function factory({", "}) {}");
}
#[test]
fn function_expression_binding() {
    signature("const factory = function({", "}) {};");
}
#[test]
fn method_binding() {
    signature("class Factory {\nmethod({", "}) {}\n}");
}
#[test]
fn interface_binding() {
    signature("interface Factory {\nmethod({", "}): void;\n}");
}
#[test]
fn default_parameter() {
    signature("function factory(value = object.call({", "})) {}");
}
#[test]
fn arrow_binding() {
    signature("const factory = ({", "}) => {};");
}
#[test]
fn return_type() {
    signature("function factory(): () => {", "} { return value; }");
}
fn type_arguments(ty: &str) {
    let text = format!("let value: {ty};\nfunction body() {{{MEMBERS}}}");
    assert_eq!(
        fold(&text),
        ReadFolderResult::Parsed {
            text,
            ranges: vec![ReadFoldRange {
                start_line: 3,
                end_line: 7,
                children: vec![]
            }]
        }
    );
}
#[test]
fn promise_arguments() {
    type_arguments("Promise<void>");
}
#[test]
fn nested_arguments() {
    type_arguments("Record<string, Array<\"x\" | \"y\">>");
}
#[test]
fn literal_arguments() {
    type_arguments("Pick<Example, 'member'>");
}
