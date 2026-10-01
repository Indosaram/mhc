use super::*;
use crate::components::markdown_token::Align;

fn inline_of(token: &Token) -> &[Token] {
    match token {
        Token::Paragraph { tokens, .. }
        | Token::Heading { tokens, .. }
        | Token::Strong { tokens, .. }
        | Token::Em { tokens, .. }
        | Token::Del { tokens, .. }
        | Token::Link { tokens, .. } => tokens.tokens(),
        _ => &[],
    }
}

#[test]
fn lexes_headings_paragraphs_and_fences() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("# Title\n\nBody text.\n\n```rust\nlet x = 1;\n```\n");
    let kinds: Vec<&str> = tokens.iter().map(Token::type_name).collect();
    assert_eq!(kinds, vec!["heading", "space", "paragraph", "space", "code"]);
    assert_eq!(tokens[0].raw(), "# Title");
    assert_eq!(tokens[2].raw(), "Body text.");
}

#[test]
fn lexes_codespans_and_strikethrough() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("Use `code` and ~~struck~~ text.\n");
    let inline = inline_of(&tokens[0]);
    let kinds: Vec<&str> = inline.iter().map(Token::type_name).collect();
    assert!(kinds.contains(&"codespan"), "{kinds:?}");
    assert!(kinds.contains(&"del"), "{kinds:?}");
}

#[test]
fn lexes_tables_with_alignment() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("| a | b |\n| --- | ---: |\n| 1 | 2 |\n");
    let Token::Table { header, align, rows, .. } = &tokens[0] else {
        panic!("expected a table, got {}", tokens[0].type_name());
    };
    assert_eq!(header.len(), 2);
    assert_eq!(align[1], Align::Right);
    assert_eq!(rows.len(), 1);
}

#[test]
fn lexes_nested_lists() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("- alpha\n- beta\n  - nested gamma\n");
    let Token::List { items, ordered, .. } = &tokens[0] else {
        panic!("expected a list, got {}", tokens[0].type_name());
    };
    assert!(!ordered);
    assert_eq!(items.len(), 2);
    let nested = match &items[1] {
        Token::ListItem { tokens, .. } => tokens
            .iter()
            .find(|token| matches!(token, Token::List { .. }))
            .expect("the second item holds a nested list"),
        other => panic!("expected a list item, got {}", other.type_name()),
    };
    assert_eq!(nested.type_name(), "list");
}

#[test]
fn resolves_inline_slots_for_blockquote_children() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("> quoted `code` text\n");
    let Token::Blockquote { tokens: children, .. } = &tokens[0] else {
        panic!("expected a blockquote, got {}", tokens[0].type_name());
    };
    let inline = children.iter().flat_map(inline_of).collect::<Vec<_>>();
    assert!(
        inline.iter().any(|token| matches!(token, Token::Codespan { .. })),
        "blockquote inline tokens were not resolved: {inline:?}"
    );
}
