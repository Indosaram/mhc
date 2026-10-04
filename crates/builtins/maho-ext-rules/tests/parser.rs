use maho_ext_rules::rules::parser::parse_rule;
#[test]
fn plain_rule_preserves_body() { let result = parse_rule("plain"); assert_eq!(result.body, "plain"); assert!(result.diagnostic.is_none()); }
#[test]
fn bom_is_removed_before_parsing() { let result = parse_rule("﻿---\nalwaysApply: true\n---\nbody"); assert_eq!(result.frontmatter.always_apply, Some(true)); }
#[test]
fn crlf_frontmatter_is_supported() { let result = parse_rule("---\r\nglobs: '*.rs'\r\n---\r\nbody"); assert_eq!(result.frontmatter.globs, vec!["*.rs"]); assert_eq!(result.body, "body"); }
#[test]
fn aliases_merge_and_deduplicate() { let result = parse_rule("---\nglobs: a\npaths: [a, b]\napplyTo: c, d\n---\nbody"); assert_eq!(result.frontmatter.globs, vec!["a","b","c","d"]); }
#[test]
fn quoted_hash_is_not_a_comment() { let result = parse_rule("---\ndescription: 'x#y' # comment\n---\nbody"); assert_eq!(result.frontmatter.description.as_deref(), Some("x#y")); }
#[test]
fn multiline_patterns_are_supported() { let result = parse_rule("---\npaths:\n  - '*.rs'\n  - '*.ts'\n---\nbody"); assert_eq!(result.frontmatter.globs, vec!["*.rs","*.ts"]); }
#[test]
fn missing_closing_delimiter_salvages_all_content() { let input = "---\nglobs: a\nbody"; let result = parse_rule(input); assert_eq!(result.body, input); assert!(result.diagnostic.is_some()); }
#[test]
fn invalid_boolean_salvages_all_content() { let input = "---\nalwaysApply: yes\n---\nbody"; let result = parse_rule(input); assert_eq!(result.body, input); assert!(result.diagnostic.is_some()); }
#[test]
fn unclosed_array_is_diagnostic() { let result = parse_rule("---\nglobs: [a\n---\nbody"); assert!(result.diagnostic.is_some()); }
#[test]
fn content_after_array_is_diagnostic() { let result = parse_rule("---\nglobs: [a] trailing\n---\nbody"); assert!(result.diagnostic.is_some()); }
#[test]
fn quoted_inline_comma_is_not_a_separator() { let result = parse_rule("---\nglobs: ['a,b', c]\n---\nbody"); assert_eq!(result.frontmatter.globs, vec!["a,b","c"]); }
#[test]
fn lone_single_quote_scalar_is_an_empty_string() {
 let result = parse_rule("---\ndescription: '\n---\nbody");
 assert_eq!(result.frontmatter.description.as_deref(), Some(""));
 assert!(result.diagnostic.is_none());
}
#[test]
fn repeated_carriage_returns_are_not_a_closing_delimiter() {
 let input = "---\ndescription: test\n---\r\r\nbody";
 let result = parse_rule(input);
 assert_eq!(result.body, input);
 assert_eq!(result.diagnostic.as_deref(), Some("Missing closing frontmatter delimiter"));
}
