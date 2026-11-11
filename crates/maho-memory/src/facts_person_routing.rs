//! Which person a fact belongs to, and when two spellings are one name.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-person-routing.md`. **Longest matching name wins**, because a
//! short alias is contained in a long one: "Indo" matches half the address book, "Indosaram" only its
//! owner. Ties go to the smallest slug so the same input always produces the same target.

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPerson {
    pub slug: String,
    /// Written onto a new card.
    pub display_name: String,
    /// Display name included.
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonClaim {
    pub name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasTie {
    pub alias: String,
    /// Slug order.
    pub slugs: Vec<String>,
    pub chosen: String,
}

/// NFKC first: the same name can arrive composed or decomposed depending on the input method, and raw
/// byte comparison would treat those as two people.
pub fn fold_alias(value: &str) -> String {
    value
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .trim()
        .to_owned()
}

/// Longest matching name wins; ties go to the smallest slug (byte order). The callback sees a tie
/// without being able to change the decision.
pub fn resolve_person_slug(
    index: &[KnownPerson],
    claim: &PersonClaim,
    on_alias_tie: Option<&mut dyn FnMut(&AliasTie)>,
) -> Option<String> {
    let inputs: Vec<String> = std::iter::once(&claim.name)
        .chain(claim.aliases.iter())
        .map(|value| fold_alias(value))
        .collect();
    let mut matches: std::collections::BTreeMap<&str, (usize, &str)> =
        std::collections::BTreeMap::new();
    for person in index {
        for name in &person.names {
            if !inputs.contains(&fold_alias(name)) {
                continue;
            }
            // UTF-16 length, not `chars()`: the stored collation came from a JS string, where an astral
            // character costs two units, and a surrogate-pair name must not outrank a longer one.
            let length = name.encode_utf16().count();
            if matches
                .get(person.slug.as_str())
                .is_none_or(|(current, _)| length > *current)
            {
                matches.insert(person.slug.as_str(), (length, name.as_str()));
            }
        }
    }
    let longest = matches.values().map(|(length, _)| *length).max()?;
    let winners: Vec<(&str, &str)> = matches
        .iter()
        .filter(|(_, (length, _))| *length == longest)
        .map(|(slug, (_, name))| (*slug, *name))
        .collect();
    let (chosen, alias) = winners[0];
    if winners.len() > 1
        && let Some(callback) = on_alias_tie
    {
        callback(&AliasTie {
            alias: (*alias).to_owned(),
            slugs: winners.iter().map(|(slug, _)| (*slug).to_owned()).collect(),
            chosen: chosen.to_owned(),
        });
    }
    Some(chosen.to_owned())
}

/// Checked after folding, so `User` and `the user` are the same self-reference.
pub fn is_self_reference(name: &str) -> bool {
    matches!(
        fold_alias(name).as_str(),
        "user" | "me" | "you" | "i" | "the user" | "human"
    )
}

/// The trailing comment is dropped because a captured observation often ends with one, and the same fact
/// recorded with and without it must not become two entries.
pub fn normalize_observation_text(text: &str) -> String {
    let folded: String = text.nfkc().collect::<String>().to_lowercase();
    let collapsed = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    let uncommented = strip_trailing_comment(&collapsed).trim();
    uncommented
        .trim_end_matches(is_punctuation)
        .trim()
        .to_owned()
}

/// Anchored at the **earliest** `<!--` whose first `-->` is final, matching the lazy regex it replaces:
/// `a <!-- b --> c -->` has no trailing comment, because the first `-->` is not at the end.
fn strip_trailing_comment(text: &str) -> &str {
    let trimmed = text.trim_end();
    if !trimmed.ends_with("-->") {
        return text;
    }
    let mut search_from = 0;
    while let Some(offset) = trimmed[search_from..].find("<!--") {
        let start = search_from + offset;
        let after_open = start + 4;
        if let Some(close) = trimmed[after_open..].find("-->")
            && after_open + close + 3 == trimmed.len()
        {
            return &trimmed[..start];
        }
        search_from = start + 4;
    }
    text
}

fn is_punctuation(ch: char) -> bool {
    matches!(
        get_general_category(ch),
        GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(slug: &str, names: &[&str]) -> KnownPerson {
        KnownPerson {
            slug: slug.to_owned(),
            display_name: names[0].to_owned(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn claim(name: &str, aliases: &[&str]) -> PersonClaim {
        PersonClaim {
            name: name.to_owned(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        }
    }

    #[test]
    fn a_single_known_name_resolves_to_its_slug() {
        let index = vec![person("indo", &["Indosaram"])];
        assert_eq!(
            resolve_person_slug(&index, &claim("Indosaram", &[]), None),
            Some("indo".to_owned())
        );
    }

    #[test]
    fn matching_ignores_case_and_composition() {
        let index = vec![person("indo", &["Indosaram"])];
        let composed = "Jose\u{0301}";
        let index2 = vec![person("jose", &[composed])];
        assert_eq!(
            resolve_person_slug(&index2, &claim("Jos\u{e9}", &[]), None),
            Some("jose".to_owned())
        );
        assert_eq!(
            resolve_person_slug(&index, &claim("INDOSARAM", &[]), None),
            Some("indo".to_owned())
        );
    }

    #[test]
    fn an_unknown_name_resolves_to_nothing() {
        let index = vec![person("indo", &["Indosaram"])];
        assert_eq!(
            resolve_person_slug(&index, &claim("Nobody", &[]), None),
            None
        );
        assert_eq!(
            resolve_person_slug(&[], &claim("Indosaram", &[]), None),
            None
        );
    }

    #[test]
    fn an_alias_on_the_claim_matches_a_name_in_the_index() {
        let index = vec![person("indo", &["Indosaram"])];
        assert_eq!(
            resolve_person_slug(&index, &claim("someone", &["indosaram"]), None),
            Some("indo".to_owned())
        );
    }

    #[test]
    fn the_longest_matching_name_wins_over_a_contained_one() {
        let index = vec![
            person("indo", &["Indo"]),
            person("indosaram", &["Indosaram"]),
        ];
        assert_eq!(
            resolve_person_slug(&index, &claim("Indosaram", &[]), None),
            Some("indosaram".to_owned())
        );
    }

    #[test]
    fn a_tie_goes_to_the_smallest_slug() {
        let index = vec![
            person("zeta", &["Same Name"]),
            person("alpha", &["Same Name"]),
        ];
        assert_eq!(
            resolve_person_slug(&index, &claim("Same Name", &[]), None),
            Some("alpha".to_owned())
        );
    }

    #[test]
    fn a_tie_is_reported_with_every_winner() {
        let index = vec![
            person("zeta", &["Same Name"]),
            person("alpha", &["Same Name"]),
        ];
        let mut seen: Vec<AliasTie> = Vec::new();
        let mut record = |tie: &AliasTie| seen.push(tie.clone());
        let resolved = resolve_person_slug(&index, &claim("Same Name", &[]), Some(&mut record));
        assert_eq!(resolved, Some("alpha".to_owned()));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].slugs, vec!["alpha".to_owned(), "zeta".to_owned()]);
        assert_eq!(seen[0].chosen, "alpha");
        assert_eq!(seen[0].alias, "Same Name");
    }

    #[test]
    fn an_uncontested_match_reports_no_tie() {
        let index = vec![person("indo", &["Indosaram"])];
        let mut count = 0;
        let mut record = |_: &AliasTie| count += 1;
        resolve_person_slug(&index, &claim("Indosaram", &[]), Some(&mut record));
        assert_eq!(count, 0);
    }

    #[test]
    fn the_longest_name_of_one_person_is_the_one_weighted() {
        let index = vec![person("a", &["Indo"]), person("b", &["Indosaram", "Indo"])];
        assert_eq!(
            resolve_person_slug(&index, &claim("Indosaram", &[]), None),
            Some("b".to_owned())
        );
    }

    #[test]
    fn a_surrogate_pair_name_is_weighted_by_utf16_units() {
        let index = vec![person("a", &["x\u{1F600}"])];
        let mut lengths = Vec::new();
        for name in &index[0].names {
            lengths.push(name.encode_utf16().count());
        }
        assert_eq!(lengths[0], 3);
        assert_eq!(
            resolve_person_slug(&index, &claim("x\u{1F600}", &[]), None),
            Some("a".to_owned())
        );
    }

    #[test]
    fn the_generic_self_references_are_recognised_in_any_case() {
        for name in ["user", "Me", "YOU", "i", "the user", "Human"] {
            assert!(is_self_reference(name), "name: {name}");
        }
        for name in ["Indosaram", "the human user", ""] {
            assert!(!is_self_reference(name), "name: {name}");
        }
    }

    #[test]
    fn normalization_folds_case_and_collapses_whitespace() {
        assert_eq!(
            normalize_observation_text("  Likes   Coffee  "),
            "likes coffee"
        );
    }

    #[test]
    fn normalization_keeps_inner_punctuation() {
        assert_eq!(
            normalize_observation_text("lives in Seoul, Korea"),
            "lives in seoul, korea"
        );
    }

    #[test]
    fn normalization_drops_trailing_punctuation_and_comment() {
        assert_eq!(normalize_observation_text("likes coffee."), "likes coffee");
        assert_eq!(
            normalize_observation_text("likes coffee <!-- captured 2026 -->"),
            "likes coffee"
        );
        assert_eq!(
            normalize_observation_text("likes coffee..."),
            "likes coffee"
        );
    }

    #[test]
    fn an_inner_comment_is_not_trailing_and_stays() {
        assert_eq!(
            normalize_observation_text("likes <!-- note --> coffee"),
            "likes <!-- note --> coffee"
        );
    }

    #[test]
    fn a_comment_whose_close_is_not_at_the_end_is_not_stripped() {
        // Not a trailing comment, so the text is left whole; only '>' is punctuation and '>' is symbol.
        assert_eq!(
            normalize_observation_text("a <!-- b --> c -->"),
            "a <!-- b --> c -->"
        );
    }

    #[test]
    fn normalizing_twice_is_stable() {
        let once = normalize_observation_text("Likes Coffee (a lot)");
        assert_eq!(normalize_observation_text(&once), once);
    }

    #[test]
    fn korean_text_is_preserved() {
        assert_eq!(
            normalize_observation_text("한국어를 좋아함."),
            "한국어를 좋아함"
        );
    }
}
