use std::collections::HashSet;

use pretty_assertions::assert_eq;

use super::*;

const DEFAULT_LIMITS: PeopleLimits = PeopleLimits {
    max_entries: 40,
    max_entry_chars: 200,
};

#[test]
fn test_parse_people_card_when_well_formed_all_prefixes_then_extracts_all_entries() {
    let text = [
        "IDENTITY: Software engineer",
        "ATTRIBUTE: Prefers concise code",
        "RELATIONSHIP: collaborates: jane-doe",
        "INSTRUCTION: Always cite sources",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert_eq!(result.diagnostics, Vec::<String>::new());
    assert_eq!(result.card.entries.len(), 4);
    assert_eq!(
        result.card.entries[0],
        CardEntry {
            prefix: "IDENTITY".to_string(),
            content: "Software engineer".to_string(),
        }
    );
    assert_eq!(
        result.card.entries[1],
        CardEntry {
            prefix: "ATTRIBUTE".to_string(),
            content: "Prefers concise code".to_string(),
        }
    );
    assert_eq!(
        result.card.entries[2],
        CardEntry {
            prefix: "RELATIONSHIP".to_string(),
            content: "collaborates: jane-doe".to_string(),
        }
    );
    assert_eq!(
        result.card.entries[3],
        CardEntry {
            prefix: "INSTRUCTION".to_string(),
            content: "Always cite sources".to_string(),
        }
    );
}

#[test]
fn test_serialize_people_card_when_well_formed_then_roundtrips_stably() {
    let text = [
        "IDENTITY: Software engineer",
        "ATTRIBUTE: Prefers concise code",
        "RELATIONSHIP: collaborates: jane-doe",
        "INSTRUCTION: Always cite sources",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);
    let serialized = serialize_people_card(&result.card, DEFAULT_LIMITS);
    let reparsed = parse_people_card(&serialized, DEFAULT_LIMITS);

    assert_eq!(reparsed.card, result.card);
}

#[test]
fn test_parse_people_card_when_blank_lines_between_entries_then_skips_blank_lines() {
    let text = ["IDENTITY: Engineer", "", "ATTRIBUTE: Likes dogs"].join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert_eq!(result.card.entries.len(), 2);
    assert_eq!(result.diagnostics, Vec::<String>::new());
}

#[test]
fn test_parse_people_card_when_comment_only_line_then_skips_comment_lines() {
    let text = [
        "IDENTITY: Engineer",
        "# this is a comment",
        "ATTRIBUTE: Likes dogs",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert_eq!(result.card.entries.len(), 2);
    assert_eq!(result.diagnostics, Vec::<String>::new());
}

#[test]
fn test_parse_people_card_when_invalid_prefix_line_then_returns_diagnostic_and_parses_valid_entries()
 {
    let text = ["IDENTITY: Engineer", "BADPREFIX: something"].join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
    assert!(result.diagnostics[0].contains("BADPREFIX"));
    assert_eq!(result.card.entries.len(), 1);
    assert_eq!(result.card.entries[0].prefix, "IDENTITY");
}

#[test]
fn test_parse_people_card_when_entry_exceeds_max_entry_chars_then_returns_diagnostic() {
    let long_content = "x".repeat(201);
    let text = format!("IDENTITY: {long_content}");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
    assert!(result.diagnostics[0].contains("200"));
}

#[test]
fn test_parse_people_card_when_entries_exceed_max_entries_then_returns_diagnostic() {
    let mut lines = Vec::new();
    for i in 0..45 {
        lines.push(format!("ATTRIBUTE: value {i}"));
    }

    let result = parse_people_card(&lines.join("\n"), DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
    assert!(result.diagnostics[0].contains("40"));
}

#[test]
fn test_parse_people_card_when_entry_at_exactly_max_entry_chars_then_accepts_without_diagnostic() {
    let content = "x".repeat(200);
    let text = format!("IDENTITY: {content}");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert_eq!(result.diagnostics, Vec::<String>::new());
    assert_eq!(result.card.entries.len(), 1);
}

#[test]
fn test_parse_people_card_when_empty_card_then_yields_no_entries_and_no_diagnostics() {
    let result = parse_people_card("", DEFAULT_LIMITS);

    assert_eq!(result.card.entries, Vec::<CardEntry>::new());
    assert_eq!(result.diagnostics, Vec::<String>::new());
}

#[test]
fn test_parse_people_card_when_trailing_whitespace_on_lines_then_trims() {
    let text = "IDENTITY: Engineer   \nATTRIBUTE: Likes dogs   ";

    let result = parse_people_card(text, DEFAULT_LIMITS);

    assert_eq!(result.card.entries[0].content, "Engineer");
    assert_eq!(result.card.entries[1].content, "Likes dogs");
}

#[test]
fn test_serialize_people_card_when_entries_given_then_produces_one_line_per_entry() {
    let card = PeopleCard {
        entries: vec![
            CardEntry {
                prefix: "IDENTITY".to_string(),
                content: "Engineer".to_string(),
            },
            CardEntry {
                prefix: "ATTRIBUTE".to_string(),
                content: "Likes dogs".to_string(),
            },
        ],
        observations: None,
    };

    let serialized = serialize_people_card(&card, DEFAULT_LIMITS);
    assert_eq!(serialized, "IDENTITY: Engineer\nATTRIBUTE: Likes dogs");
}

#[test]
fn test_serialize_people_card_when_empty_card_then_produces_empty_string() {
    let card = PeopleCard::default();
    let serialized = serialize_people_card(&card, DEFAULT_LIMITS);
    assert_eq!(serialized, "");
}

#[test]
fn test_parse_people_card_when_relationship_entry_then_preserves_predicate_and_content() {
    let text = "RELATIONSHIP: works-with: jane-doe";

    let result = parse_people_card(text, DEFAULT_LIMITS);

    assert_eq!(result.card.entries[0].prefix, "RELATIONSHIP");
    assert_eq!(result.card.entries[0].content, "works-with: jane-doe");
}

#[test]
fn test_parse_people_card_when_prefix_without_content_then_returns_diagnostic() {
    let text = "IDENTITY:";

    let result = parse_people_card(text, DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
}

#[test]
fn test_parse_people_card_when_prefix_with_whitespace_content_then_returns_diagnostic() {
    let text = "IDENTITY:   ";

    let result = parse_people_card(text, DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
}

#[test]
fn test_parse_people_card_when_explicit_observation_with_full_comment_fields_then_extracts_all() {
    let text = [
        "## Explicit",
        "",
        "- [2026-01-15] Prefers dark mode <!-- src: msg-001; n=2; pattern: repeated; confidence: high; status: open -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert_eq!(result.diagnostics, Vec::<String>::new());
    let obs = result.card.observations.as_ref().unwrap();
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].section, "Explicit");
    assert_eq!(obs[0].entries[0].date, "2026-01-15");
    assert_eq!(obs[0].entries[0].content, "Prefers dark mode");
    assert_eq!(obs[0].entries[0].src, Some("msg-001".to_string()));
    assert_eq!(obs[0].entries[0].n, Some(2));
    assert_eq!(obs[0].entries[0].pattern, Some("repeated".to_string()));
    assert_eq!(obs[0].entries[0].confidence, Some("high".to_string()));
    assert_eq!(obs[0].entries[0].status, Some("open".to_string()));
}

#[test]
fn test_parse_people_card_when_observation_with_minimal_fields_then_leaves_optional_fields_none() {
    let text = [
        "## Explicit",
        "",
        "- [2026-01-15] Some fact <!-- src: msg-001 -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);
    let obs = result.card.observations.as_ref().unwrap();

    assert_eq!(obs[0].entries[0].src, Some("msg-001".to_string()));
    assert_eq!(obs[0].entries[0].n, None);
    assert_eq!(obs[0].entries[0].pattern, None);
    assert_eq!(obs[0].entries[0].confidence, None);
    assert_eq!(obs[0].entries[0].status, None);
}

#[test]
fn test_parse_people_card_when_observation_with_no_comment_then_leaves_comment_fields_none() {
    let text = ["## Explicit", "", "- [2026-01-15] Plain fact"].join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);
    let obs = result.card.observations.as_ref().unwrap();

    assert_eq!(obs[0].entries[0].content, "Plain fact");
    assert_eq!(obs[0].entries[0].src, None);
}

#[test]
fn test_parse_people_card_when_multiple_observation_sections_then_separates_by_section() {
    let text = [
        "## Explicit",
        "",
        "- [2026-01-15] Fact A <!-- src: a -->",
        "",
        "## Deductive",
        "",
        "- [2026-01-16] Inference B <!-- src: b; confidence: medium -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);
    let obs = result.card.observations.as_ref().unwrap();

    assert_eq!(obs.len(), 2);
    assert_eq!(obs[0].section, "Explicit");
    assert_eq!(obs[0].entries.len(), 1);
    assert_eq!(obs[1].section, "Deductive");
    assert_eq!(obs[1].entries.len(), 1);
}

#[test]
fn test_parse_people_card_when_observation_with_n_but_no_src_then_extracts_n() {
    let text = [
        "## Explicit",
        "",
        "- [2026-01-15] Reinforced fact <!-- n=3 -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);
    let obs = result.card.observations.as_ref().unwrap();

    assert_eq!(obs[0].entries[0].n, Some(3));
    assert_eq!(obs[0].entries[0].src, None);
}

#[test]
fn test_parse_people_card_when_observation_with_multiple_src_ids_then_preserves_full_src() {
    let text = [
        "## Explicit",
        "",
        "- [2026-01-15] Multi-source fact <!-- src: msg-001,msg-002; n=2 -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);
    let obs = result.card.observations.as_ref().unwrap();

    assert_eq!(obs[0].entries[0].src, Some("msg-001,msg-002".to_string()));
    assert_eq!(obs[0].entries[0].n, Some(2));
}

#[test]
fn test_serialize_people_card_when_observations_given_then_produces_parseable_card() {
    let card = PeopleCard {
        entries: vec![CardEntry {
            prefix: "IDENTITY".to_string(),
            content: "Engineer".to_string(),
        }],
        observations: Some(vec![ObservationGroup {
            section: "Explicit".to_string(),
            entries: vec![ObservationEntry {
                date: "2026-01-15".to_string(),
                content: "Prefers dark mode".to_string(),
                src: Some("msg-001".to_string()),
                n: Some(2),
                pattern: Some("repeated".to_string()),
                confidence: Some("high".to_string()),
                status: Some("open".to_string()),
            }],
        }]),
    };

    let serialized = serialize_people_card(&card, DEFAULT_LIMITS);
    let reparsed = parse_people_card(&serialized, DEFAULT_LIMITS);

    assert_eq!(reparsed.card.entries, card.entries);
    assert_eq!(reparsed.card.observations, card.observations);
}

#[test]
fn test_people_card_when_mixed_entries_and_observations_then_parses_and_roundtrips() {
    let text = [
        "IDENTITY: Engineer",
        "ATTRIBUTE: Likes dogs",
        "",
        "## Explicit",
        "",
        "- [2026-01-15] Fact <!-- src: m1 -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert_eq!(result.card.entries.len(), 2);
    assert_eq!(result.card.observations.as_ref().unwrap().len(), 1);
    assert_eq!(result.diagnostics, Vec::<String>::new());

    let serialized = serialize_people_card(&result.card, DEFAULT_LIMITS);
    let reparsed = parse_people_card(&serialized, DEFAULT_LIMITS);

    assert_eq!(reparsed.card, result.card);
}

#[test]
fn test_parse_people_card_when_unknown_observation_section_then_returns_diagnostic() {
    let text = [
        "## Explicit",
        "",
        "- [2026-01-15] Fact <!-- src: m1 -->",
        "",
        "## Unknown",
        "",
        "- [2026-01-16] Mystery <!-- src: m2 -->",
    ]
    .join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
    assert!(result.diagnostics[0].contains("Unknown"));
}

#[test]
fn test_parse_people_card_when_observation_without_date_bracket_then_returns_diagnostic() {
    let text = ["## Explicit", "", "- No date fact <!-- src: m1 -->"].join("\n");

    let result = parse_people_card(&text, DEFAULT_LIMITS);

    assert!(!result.diagnostics.is_empty());
}

#[test]
fn test_sanitize_person_slug_when_display_name_given_then_matches_slug_rules() {
    assert_eq!(sanitize_person_slug("Yeongyu Park"), "yeongyu-park");
    assert_eq!(sanitize_person_slug("  spaced  "), "spaced");
    assert_eq!(sanitize_person_slug("Hello!@#World"), "hello-world");
    assert_eq!(sanitize_person_slug(&"a".repeat(50)).len(), 40);
}

#[test]
fn test_is_reserved_slug_when_checked_then_flags_human_only() {
    assert!(is_reserved_slug("human"));
    assert!(is_reserved_slug("HUMAN"));
    assert!(!is_reserved_slug("yeongyu"));
}

#[test]
fn test_resolve_slug_collision_when_collisions_exist_then_appends_numeric_suffix() {
    let mut existing = HashSet::new();
    existing.insert("yeongyu".to_string());
    existing.insert("yeongyu-2".to_string());
    assert_eq!(resolve_slug_collision("yeongyu", &existing), "yeongyu-3");

    let mut single = HashSet::new();
    single.insert("yeongyu".to_string());
    assert_eq!(resolve_slug_collision("yeongyu", &single), "yeongyu-2");

    let mut partial = HashSet::new();
    partial.insert("yeongyu-2".to_string());
    partial.insert("yeongyu-3".to_string());
    assert_eq!(resolve_slug_collision("yeongyu", &partial), "yeongyu");

    let empty = HashSet::new();
    assert_eq!(resolve_slug_collision("yeongyu", &empty), "yeongyu");
}
