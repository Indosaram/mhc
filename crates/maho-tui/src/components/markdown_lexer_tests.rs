use super::*;

#[test]
fn lexes_a_paragraph_and_a_heading() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("# Title\n\nHello *world*.\n");
    assert_eq!(tokens[0].type_name(), "heading");
    assert_eq!(tokens[0].raw(), "# Title");
}
