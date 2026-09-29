use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::*;
use crate::facts::person_index::FactsPeopleIndexEntry;
use crate::people::PeopleLimits;

#[test]
fn test_normalize_observation_text() {
    assert_eq!(
        normalize_observation_text("  Mina   prefers   concise reviews.  "),
        "mina prefers concise reviews"
    );
    assert_eq!(
        normalize_observation_text("Prefers concise reviews! <!-- ignored comment -->"),
        "prefers concise reviews"
    );
    assert_eq!(normalize_observation_text("Loves Rust???"), "loves rust");
}

#[test]
fn test_resolve_person_slug_by_primary_name() {
    let index = vec![
        FactsPeopleIndexEntry {
            slug: "alice-smith".to_string(),
            display_name: "Alice Smith".to_string(),
            names: vec!["Alice Smith".to_string(), "Ali".to_string()],
        },
        FactsPeopleIndexEntry {
            slug: "bob-jones".to_string(),
            display_name: "Bob Jones".to_string(),
            names: vec!["Bob Jones".to_string()],
        },
    ];

    let person = FactsPersonReference {
        name: "alice smith".to_string(),
        aliases: Vec::new(),
    };

    let resolved = resolve_person_slug(&index, &person, None);
    assert_eq!(resolved, Some("alice-smith".to_string()));
}

#[test]
fn test_resolve_person_slug_longest_alias_wins() {
    let index = vec![
        FactsPeopleIndexEntry {
            slug: "shorty".to_string(),
            display_name: "Short".to_string(),
            names: vec!["Al".to_string()],
        },
        FactsPeopleIndexEntry {
            slug: "longy".to_string(),
            display_name: "Long".to_string(),
            names: vec!["Alexander".to_string()],
        },
    ];

    let person = FactsPersonReference {
        name: "User".to_string(),
        aliases: vec!["Al".to_string(), "Alexander".to_string()],
    };

    let resolved = resolve_person_slug(&index, &person, None);
    assert_eq!(resolved, Some("longy".to_string()));
}

#[test]
fn test_resolve_person_slug_tie_breaker_lexicographic() {
    let index = vec![
        FactsPeopleIndexEntry {
            slug: "beta-user".to_string(),
            display_name: "Beta".to_string(),
            names: vec!["SameAlias".to_string()],
        },
        FactsPeopleIndexEntry {
            slug: "alpha-user".to_string(),
            display_name: "Alpha".to_string(),
            names: vec!["SameAlias".to_string()],
        },
    ];

    let person = FactsPersonReference {
        name: "SameAlias".to_string(),
        aliases: Vec::new(),
    };

    let mut tie_observed: Option<FactsAliasTie> = None;
    let resolved = resolve_person_slug(
        &index,
        &person,
        Some(&mut |tie: &FactsAliasTie| {
            tie_observed = Some(tie.clone());
        }),
    );

    assert_eq!(resolved, Some("alpha-user".to_string()));
    assert_eq!(tie_observed.is_some(), true);
    let tie = tie_observed.unwrap();
    assert_eq!(tie.chosen, "alpha-user");
    assert_eq!(tie.slugs, vec!["alpha-user", "beta-user"]);
}

#[test]
fn test_render_card_skeleton() {
    let target = FactsPersonTarget {
        slug: "alice".to_string(),
        display_name: "Alice".to_string(),
        is_new: true,
        person: Some(FactsPersonReference {
            name: "Alice Smith".to_string(),
            aliases: vec!["Ali".to_string()],
        }),
    };

    let rendered = render_card_skeleton(&target).expect("card skeleton");
    assert_eq!(
        rendered,
        "---\ndescription: Person - Alice Smith\nkind: person\naliases: [\"Ali\"]\n---\n"
    );
}

#[test]
fn test_render_person_targets_with_reinforcement() {
    let tmp = tempdir().expect("tempdir");
    let mut plan = FactsRoutingPlan::default();

    let target = FactsPersonTarget {
        slug: "mina".to_string(),
        display_name: "Mina".to_string(),
        is_new: true,
        person: Some(FactsPersonReference {
            name: "Mina Lee".to_string(),
            aliases: vec!["Min".to_string()],
        }),
    };

    let records = vec![
        FactsExtractionRecord::Person {
            person: FactsPersonReference {
                name: "Mina Lee".to_string(),
                aliases: vec![],
            },
            text: "Prefers concise reviews.".to_string(),
            date: "2026-08-10".to_string(),
        },
        FactsExtractionRecord::Person {
            person: FactsPersonReference {
                name: "Mina Lee".to_string(),
                aliases: vec![],
            },
            text: "Prefers concise reviews!".to_string(),
            date: "2026-08-11".to_string(),
        },
    ];

    plan.observations
        .insert("mina".to_string(), ObservationBucket { target, records });

    let limits = PeopleLimits {
        max_entries: 50,
        max_entry_chars: 500,
    };

    let targets = render_person_targets(tmp.path(), &plan, limits).expect("render");
    assert_eq!(targets.contains_key("people/mina/card.md"), true);
    assert_eq!(targets.contains_key("people/mina/observations.md"), true);

    let obs_content = targets.get("people/mina/observations.md").unwrap();
    assert_eq!(
        obs_content.contains("- [2026-08-11] Prefers concise reviews. <!-- n=2 -->"),
        true
    );
}
