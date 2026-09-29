use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_parse_memory_file_when_well_formed_then_extracts_description_and_body() {
    let content = "---\ndescription: A note about the project\n---\nThis is the body.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.description, "A note about the project");
    assert_eq!(parsed.frontmatter.read_only, None);
    assert_eq!(parsed.body, "This is the body.");
}

#[test]
fn test_render_memory_file_when_well_formed_then_roundtrips_stably() {
    let content = "---\ndescription: A note about the project\n---\nThis is the body.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();

    assert_eq!(parsed, parse_memory_file(&rendered).unwrap());
    assert_eq!(rendered, content);
}

#[test]
fn test_parse_memory_file_when_read_only_true_then_preserves_read_only() {
    let content = "---\ndescription: Locked note\nread_only: true\n---\nBody text.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.read_only, Some("true".to_string()));
    assert_eq!(parsed.body, "Body text.");
}

#[test]
fn test_render_memory_file_when_read_only_edited_then_preserves_read_only() {
    let content = "---\ndescription: Locked note\nread_only: true\n---\nBody text.";
    let parsed = parse_memory_file(content).unwrap();
    let new_body = "Updated body content.";
    let rendered = render_memory_file(&parsed.frontmatter, new_body).unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();

    assert_eq!(reparsed.frontmatter.read_only, Some("true".to_string()));
    assert_eq!(reparsed.frontmatter.description, "Locked note");
    assert_eq!(reparsed.body, new_body);
}

#[test]
fn test_parse_memory_file_when_limit_key_present_then_tolerates_and_ignores_limit() {
    let content = "---\ndescription: Has limit\nlimit: 5000\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.description, "Has limit");
    assert_eq!(parsed.frontmatter.read_only, None);
    assert_eq!(parsed.body, "Body.");
}

#[test]
fn test_parse_memory_file_when_crlf_line_endings_then_normalizes_crlf() {
    let content = "---\r\ndescription: Windows file\r\nread_only: false\r\n---\r\nCRLF body.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.description, "Windows file");
    assert_eq!(parsed.frontmatter.read_only, Some("false".to_string()));
    assert_eq!(parsed.body, "CRLF body.");
}

#[test]
fn test_render_memory_file_when_crlf_input_then_outputs_lf_only() {
    let content = "---\r\ndescription: Windows file\r\nread_only: false\r\n---\r\nCRLF body.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();

    assert!(!rendered.contains("\r\n"));
    assert!(rendered.contains('\n'));
}

#[test]
fn test_render_memory_file_when_description_has_newlines_then_collapses_to_spaces() {
    let multiline_description = "Line one\nLine two\r\nLine three";
    let rendered = render_memory_file(
        &MemoryFrontmatter {
            description: multiline_description.to_string(),
            read_only: Some("true".to_string()),
            kind: None,
            aliases: None,
        },
        "Body.",
    )
    .unwrap();

    let reparsed = parse_memory_file(&rendered).unwrap();
    assert_eq!(
        reparsed.frontmatter.description,
        "Line one Line two Line three"
    );
}

#[test]
fn test_parse_memory_file_when_body_is_empty_then_yields_empty_body() {
    let content = "---\ndescription: Just frontmatter\n---\n";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.body, "");
}

#[test]
fn test_render_memory_file_when_body_is_empty_then_produces_trailing_newline() {
    let rendered = render_memory_file(
        &MemoryFrontmatter {
            description: "Just frontmatter".to_string(),
            read_only: None,
            kind: None,
            aliases: None,
        },
        "",
    )
    .unwrap();

    assert_eq!(rendered, "---\ndescription: Just frontmatter\n---\n");
}

#[test]
fn test_parse_memory_file_when_missing_closing_delimiter_then_throws_frontmatter_error() {
    let content = "---\ndescription: Missing close\nbody text";
    let err = parse_memory_file(content).unwrap_err();
    assert_eq!(
        err.message,
        "frontmatter: target file is missing required frontmatter"
    );
}

#[test]
fn test_parse_memory_file_when_no_frontmatter_at_all_then_throws_frontmatter_error() {
    let content = "Just some plain text.\nNo frontmatter here.";
    let err = parse_memory_file(content).unwrap_err();
    assert_eq!(
        err.message,
        "frontmatter: target file is missing required frontmatter"
    );
}

#[test]
fn test_parse_memory_file_when_missing_description_then_throws_frontmatter_error() {
    let content = "---\nread_only: true\n---\nBody.";
    let err = parse_memory_file(content).unwrap_err();
    assert_eq!(
        err.message,
        "frontmatter: target file frontmatter is missing 'description'"
    );
}

#[test]
fn test_parse_memory_file_when_empty_description_value_then_throws_frontmatter_error() {
    let content = "---\ndescription:   \n---\nBody.";
    let err = parse_memory_file(content).unwrap_err();
    assert_eq!(
        err.message,
        "frontmatter: target file frontmatter is missing 'description'"
    );
}

#[test]
fn test_render_memory_file_when_empty_description_then_throws_frontmatter_error() {
    let err = render_memory_file(
        &MemoryFrontmatter {
            description: "   ".to_string(),
            read_only: None,
            kind: None,
            aliases: None,
        },
        "body",
    )
    .unwrap_err();
    assert_eq!(err.message, "frontmatter: 'description' must not be empty");
}

#[test]
fn test_render_memory_file_when_whitespace_only_description_then_throws_frontmatter_error() {
    let err = render_memory_file(
        &MemoryFrontmatter {
            description: "\n\t  \n".to_string(),
            read_only: None,
            kind: None,
            aliases: None,
        },
        "body",
    )
    .unwrap_err();
    assert_eq!(err.message, "frontmatter: 'description' must not be empty");
}

#[test]
fn test_frontmatter_when_varied_inputs_given_then_roundtrip_is_stable() {
    let cases = [
        ParsedMemoryFile {
            frontmatter: MemoryFrontmatter {
                description: "Simple".to_string(),
                read_only: None,
                kind: None,
                aliases: None,
            },
            body: "Body one.".to_string(),
        },
        ParsedMemoryFile {
            frontmatter: MemoryFrontmatter {
                description: "With read_only".to_string(),
                read_only: Some("true".to_string()),
                kind: None,
                aliases: None,
            },
            body: "Body two.\nSecond line.".to_string(),
        },
        ParsedMemoryFile {
            frontmatter: MemoryFrontmatter {
                description: "Multi-word description here".to_string(),
                read_only: None,
                kind: None,
                aliases: None,
            },
            body: String::new(),
        },
        ParsedMemoryFile {
            frontmatter: MemoryFrontmatter {
                description: "Special chars: !@#$%^&*()".to_string(),
                read_only: Some("false".to_string()),
                kind: None,
                aliases: None,
            },
            body: "Body with special content.\n\nNew paragraph.".to_string(),
        },
    ];

    for original in cases {
        let rendered = render_memory_file(&original.frontmatter, &original.body).unwrap();
        let reparsed = parse_memory_file(&rendered).unwrap();
        assert_eq!(reparsed, original);
    }
}

#[test]
fn test_render_memory_file_when_description_has_surrounding_whitespace_then_trims() {
    let rendered = render_memory_file(
        &MemoryFrontmatter {
            description: "  spaced  ".to_string(),
            read_only: None,
            kind: None,
            aliases: None,
        },
        "body",
    )
    .unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();
    assert_eq!(reparsed.frontmatter.description, "spaced");
}

#[test]
fn test_parse_memory_file_when_extra_whitespace_around_keys_then_trims() {
    let content = "---\n  description  :   Padded value  \nread_only:  true  \n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.description, "Padded value");
    assert_eq!(parsed.frontmatter.read_only, Some("true".to_string()));
}

#[test]
fn test_parse_memory_file_when_kind_and_aliases_given_then_extracts_both() {
    let content = "---\ndescription: Person - Yeongyu\nkind: person\naliases: [\"Yeongyu\",\"YG\"]\n---\nBody text.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.description, "Person - Yeongyu");
    assert_eq!(parsed.frontmatter.kind, Some("person".to_string()));
    assert_eq!(
        parsed.frontmatter.aliases,
        Some(vec!["Yeongyu".to_string(), "YG".to_string()])
    );
    assert_eq!(parsed.body, "Body text.");
}

#[test]
fn test_render_memory_file_when_kind_and_aliases_given_then_emits_kind_then_aliases_after_description()
 {
    let content = "---\ndescription: Person - Yeongyu\nkind: person\naliases: [\"Yeongyu\",\"YG\"]\n---\nBody text.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();

    assert!(rendered.contains("description: Person - Yeongyu"));
    assert!(rendered.contains("kind: person"));
    assert!(rendered.contains("aliases: [\"Yeongyu\",\"YG\"]"));

    let desc_idx = rendered.find("description:").unwrap();
    let kind_idx = rendered.find("kind:").unwrap();
    let aliases_idx = rendered.find("aliases:").unwrap();
    assert!(desc_idx < kind_idx);
    assert!(kind_idx < aliases_idx);
}

#[test]
fn test_frontmatter_when_kind_and_aliases_given_then_roundtrip_is_stable() {
    let content = "---\ndescription: Person - Yeongyu\nkind: person\naliases: [\"Yeongyu\",\"YG\"]\n---\nBody text.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();

    assert_eq!(reparsed, parsed);
}

#[test]
fn test_parse_memory_file_when_kind_without_aliases_then_leaves_aliases_none() {
    let content = "---\ndescription: Person - Solo\nkind: person\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.kind, Some("person".to_string()));
    assert_eq!(parsed.frontmatter.aliases, None);
}

#[test]
fn test_render_memory_file_when_aliases_none_then_omits_aliases() {
    let content = "---\ndescription: Person - Solo\nkind: person\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();

    assert!(!rendered.contains("aliases:"));
}

#[test]
fn test_parse_memory_file_when_aliases_without_kind_then_leaves_kind_none() {
    let content = "---\ndescription: Person - Aliased\naliases: [\"A\",\"B\"]\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.kind, None);
    assert_eq!(
        parsed.frontmatter.aliases,
        Some(vec!["A".to_string(), "B".to_string()])
    );
}

#[test]
fn test_render_memory_file_when_kind_none_then_omits_kind() {
    let content = "---\ndescription: Person - Aliased\naliases: [\"A\",\"B\"]\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();

    assert!(!rendered.contains("kind:"));
}

#[test]
fn test_parse_memory_file_when_read_only_kind_aliases_together_then_preserves_all() {
    let content = "---\ndescription: Full frontmatter\nread_only: true\nkind: person\naliases: [\"X\"]\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();

    assert_eq!(parsed.frontmatter.description, "Full frontmatter");
    assert_eq!(parsed.frontmatter.read_only, Some("true".to_string()));
    assert_eq!(parsed.frontmatter.kind, Some("person".to_string()));
    assert_eq!(parsed.frontmatter.aliases, Some(vec!["X".to_string()]));
}

#[test]
fn test_render_memory_file_when_read_only_kind_aliases_together_then_emits_all_in_order() {
    let content = "---\ndescription: Full frontmatter\nread_only: true\nkind: person\naliases: [\"X\"]\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();

    assert_eq!(reparsed, parsed);
}

#[test]
fn test_parse_memory_file_when_aliases_not_json_array_then_throws_frontmatter_error() {
    let content = "---\ndescription: Bad aliases\naliases: Yeongyu\n---\nBody.";
    assert!(parse_memory_file(content).is_err());
}

#[test]
fn test_parse_memory_file_when_aliases_has_empty_string_then_throws_frontmatter_error() {
    let content = "---\ndescription: Bad aliases\naliases: [\"A\",\"\"]\n---\nBody.";
    assert!(parse_memory_file(content).is_err());
}

#[test]
fn test_parse_memory_file_when_aliases_has_non_string_then_throws_frontmatter_error() {
    let content = "---\ndescription: Bad aliases\naliases: [1,2,3]\n---\nBody.";
    assert!(parse_memory_file(content).is_err());
}

#[test]
fn test_parse_memory_file_when_aliases_is_json_object_then_throws_frontmatter_error() {
    let content = "---\ndescription: Bad aliases\naliases: {\"key\":\"val\"}\n---\nBody.";
    assert!(parse_memory_file(content).is_err());
}

#[test]
fn test_frontmatter_when_body_edited_then_preserves_kind_and_aliases() {
    let content = "---\ndescription: Person - Yeongyu\nkind: person\naliases: [\"Yeongyu\",\"YG\"]\n---\nIDENTITY: Software engineer";
    let parsed = parse_memory_file(content).unwrap();
    let new_body = parsed.body.replace("Software engineer", "Senior engineer");
    let rendered = render_memory_file(&parsed.frontmatter, &new_body).unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();

    assert_eq!(reparsed.frontmatter.kind, Some("person".to_string()));
    assert_eq!(
        reparsed.frontmatter.aliases,
        Some(vec!["Yeongyu".to_string(), "YG".to_string()])
    );
    assert_eq!(reparsed.body, "IDENTITY: Senior engineer");
}

#[test]
fn test_frontmatter_when_empty_aliases_array_then_accepts_and_roundtrips() {
    let content = "---\ndescription: Empty aliases\naliases: []\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();
    assert_eq!(parsed.frontmatter.aliases, Some(vec![]));

    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();
    assert_eq!(reparsed, parsed);
}

#[test]
fn test_parse_memory_file_when_kind_multi_word_then_preserves_verbatim() {
    let content = "---\ndescription: Typed\nkind: something complex\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();
    assert_eq!(
        parsed.frontmatter.kind,
        Some("something complex".to_string())
    );
}

#[test]
fn test_parse_memory_file_when_aliases_has_special_chars_then_preserves_through_roundtrip() {
    let content =
        "---\ndescription: Special\naliases: [\"O'Brien\",\"Name with spaces\"]\n---\nBody.";
    let parsed = parse_memory_file(content).unwrap();
    let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).unwrap();
    let reparsed = parse_memory_file(&rendered).unwrap();

    assert_eq!(
        reparsed.frontmatter.aliases,
        Some(vec!["O'Brien".to_string(), "Name with spaces".to_string()])
    );
}
