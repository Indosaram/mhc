use maho_ext_pi_rules::rules::{parser::parse_rule, types::{PatternList, RuleFrontmatter}};

fn frontmatter(description: Option<&str>, globs: Option<PatternList>, always_apply: Option<bool>) -> RuleFrontmatter {
    RuleFrontmatter { description: description.map(str::to_owned), globs, always_apply, ..RuleFrontmatter::default() }
}
fn single(s: &str) -> Option<PatternList> { Some(PatternList::Single(s.into())) }
fn multiple(s: &[&str]) -> Option<PatternList> { Some(PatternList::Multiple(s.iter().map(|s| (*s).into()).collect())) }
fn check(content: &str, expected: RuleFrontmatter, body: &str) {
    let result = parse_rule(content);
    assert_eq!(result.frontmatter, expected); assert_eq!(result.body, body); assert_eq!(result.diagnostic, None);
}
#[test] fn no_frontmatter() { let c = "Use strict TypeScript.\n---\nKeep this horizontal rule."; check(c, frontmatter(None,None,None), c); }
#[test] fn closing_delimiter_removes_only_one_carriage_return() { let content="---\nalwaysApply: true\n---\r\r\nbody";let result=parse_rule(content);assert!(result.diagnostic.is_some());assert_eq!(result.body,content);assert_eq!(result.frontmatter,RuleFrontmatter::default()); }
#[test] fn scalar_empty_glob_is_preserved(){check("---\nglobs: \"\"\n---\nbody",frontmatter(None,single(""),None),"body");}
#[test] fn javascript_whitespace_preserves_next_line_and_trims_bom(){check("---\n\u{feff}alwaysApply: true\ndescription: \u{0085}hello\u{0085}\n---\nbody",frontmatter(Some("\u{0085}hello\u{0085}"),None,Some(true)),"body");}
#[test] fn valid_frontmatter() { check("---\ndescription: TypeScript rules\nglobs: [\"**/*.ts\", \"**/*.tsx\"]\nalwaysApply: true\n---\nUse type-only imports.", frontmatter(Some("TypeScript rules"),multiple(&["**/*.ts","**/*.tsx"]),Some(true)), "Use type-only imports."); }
#[test] fn empty_frontmatter() { check("---\n---\n", frontmatter(None,None,None), ""); }
#[test] fn malformed_is_salvaged() { let c="---\nglobs: [broken\n---\nbody"; let r=parse_rule(c); assert_eq!(r.body,c); assert_eq!(r.frontmatter,RuleFrontmatter::default()); assert!(r.diagnostic.is_some()); }
#[test] fn missing_close_is_salvaged() { let c="---\ndescription: Missing close\nUse this as body."; let r=parse_rule(c); assert_eq!(r.body,c); assert!(r.diagnostic.is_some()); }
#[test] fn frontmatter_only() { check("---\nalwaysApply: true\ndescription: Global\n---",frontmatter(Some("Global"),None,Some(true)), ""); }
#[test] fn crlf_preserved() { check("---\r\nglobs: **/*.ts\r\n---\r\nLine one\r\nLine two",frontmatter(None,single("**/*.ts"),None),"Line one\r\nLine two"); }
#[test] fn bom_removed() { check("\u{feff}---\ndescription: BOM rule\n---\nBody without BOM.",frontmatter(Some("BOM rule"),None,None),"Body without BOM."); }
#[test] fn body_markers_preserved() { check("---\ndescription: Horizontal body\n---\nBefore\n---\nAfter",frontmatter(Some("Horizontal body"),None,None),"Before\n---\nAfter"); }
#[test] fn inline_array() { check("---\nglobs: [\"**/*.ts\", \"**/*.tsx\"]\n---\nbody",frontmatter(None,multiple(&["**/*.ts","**/*.tsx"]),None),"body"); }
#[test] fn comma_list() { check("---\nglobs: **/*.ts, **/*.tsx\n---\nbody",frontmatter(None,multiple(&["**/*.ts","**/*.tsx"]),None),"body"); }
#[test] fn multiline_array() { check("---\nglobs:\n  - **/*.ts\n  - **/*.tsx\n---\nbody",frontmatter(None,multiple(&["**/*.ts","**/*.tsx"]),None),"body"); }
#[test] fn paths_alias() { check("---\npaths: [\"src/**/*.ts\", \"test/**/*.ts\"]\n---\nbody",frontmatter(None,multiple(&["src/**/*.ts","test/**/*.ts"]),None),"body"); }
#[test] fn apply_to_alias() { check("---\napplyTo: \"src/**/*.tsx\"\n---\nbody",frontmatter(None,single("src/**/*.tsx"),None),"body"); }
#[test] fn aliases_deduplicate_in_order() { check("---\nglobs: [\"src/**/*.ts\", \"src/**/*.tsx\"]\npaths: [\"src/**/*.ts\", \"scripts/**/*.ts\"]\napplyTo: scripts/**/*.ts, docs/**/*.md\n---\nbody",frontmatter(None,multiple(&["src/**/*.ts","src/**/*.tsx","scripts/**/*.ts","docs/**/*.md"]),None),"body"); }
#[test] fn always_true() { check("---\nalwaysApply: true\n---\nbody",frontmatter(None,None,Some(true)),"body"); }
#[test] fn always_false() { check("---\nalwaysApply: false\n---\nbody",frontmatter(None,None,Some(false)),"body"); }
#[test] fn json_quoted() { check("---\ndescription: \"Use \\\"strict\\\" mode\"\n---\nbody",frontmatter(Some("Use \"strict\" mode"),None,None),"body"); }
#[test] fn unicode_body() { check("---\ndescription: Unicode\n---\nPreserve unicode 世界 and emoji ✨ in the body.",frontmatter(Some("Unicode"),None,None),"Preserve unicode 世界 and emoji ✨ in the body."); }
#[test] fn single_glob() { check("---\nglobs: **/*.md\n---\nbody",frontmatter(None,single("**/*.md"),None),"body"); }
#[test] fn unquoted_description() { check("---\ndescription: Plain rule description\n---\nbody",frontmatter(Some("Plain rule description"),None,None),"body"); }
#[test] fn unknown_fields_ignored() { check("---\ndescription: Known\nunknown: ignored\n---\nbody",frontmatter(Some("Known"),None,None),"body"); }
#[test] fn comments_removed() { check("---\n# top comment\ndescription: Commented # trailing comment\nglobs: [\"**/*.ts\", \"**/*.tsx\"] # trailing comment\n---\nbody",frontmatter(Some("Commented"),multiple(&["**/*.ts","**/*.tsx"]),None),"body"); }
