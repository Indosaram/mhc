//! Reading the people index a memory repository already holds on disk.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-person-index.md`. `resolve_person_slug` routes a fact to a
//! person by comparing names against `KnownPerson` values the caller supplies. Nothing built those values
//! from the repository, so any caller would have had to pass an empty slice — and an empty index does not
//! fail, it just resolves every claim to `None` and quietly drops the fact.
//!
//! The repository layout this reads is the one the agent already uses:
//!
//! ```text
//! system/human.md            the person the agent works with
//! people/<slug>/card.md      everyone else, one directory per slug
//! ```
//!
//! Each card is markdown with YAML frontmatter. Only two keys matter: `description`, which is the display
//! name, and `aliases`, an inline JSON array of other spellings.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::facts_person_routing::KnownPerson;

/// One person as the index reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPeopleIndexEntry {
    pub slug: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub names: Vec<String>,
}

struct CardFrontmatter {
    description: String,
    aliases: Vec<String>,
}

/// The display name a card's `description` implies, or `fallback` when it says nothing usable.
///
/// Cards the agent itself writes are described as `Person - <name>`, so the prefix is stripped to keep the
/// slug's own capitalisation out of the name list. A description that is only the prefix, or only
/// whitespace, is not a name: falling back to the slug keeps the person addressable by their directory.
pub fn display_name_of(description: &str, fallback: &str) -> String {
    let stripped = match strip_person_prefix(description) {
        Some(rest) => rest.trim(),
        None => description.trim(),
    };
    if stripped.is_empty() {
        fallback.to_string()
    } else {
        stripped.to_string()
    }
}

/// Every person this repository knows about: the human first, then `people/<slug>` in name order.
///
/// A card that is missing, unreadable, or has no `description` is skipped rather than reported: the index
/// is a routing aid, and half a card cannot route anything. The sort makes the returned order independent
/// of the filesystem's, so two runs over the same repository agree.
pub fn read_facts_people_index(repo_dir: &Path) -> Vec<FactsPeopleIndexEntry> {
    let mut index = Vec::new();
    let human_path = repo_dir.join("system").join("human.md");
    if let Some(human) = read_card_frontmatter(&human_path) {
        let display_name = display_name_of(&human.description, "human");
        let mut names = vec![display_name.clone()];
        names.extend(human.aliases);
        index.push(FactsPeopleIndexEntry {
            slug: "human".to_string(),
            display_name,
            names,
        });
    }

    let people_dir = repo_dir.join("people");
    if let Ok(read_dir) = std::fs::read_dir(&people_dir) {
        let mut subdirs = Vec::new();
        for entry in read_dir.flatten() {
            if let Ok(file_type) = entry.file_type()
                && file_type.is_dir()
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if name != "human" {
                    subdirs.push(name);
                }
            }
        }
        subdirs.sort();
        for name in subdirs {
            let card_path = people_dir.join(&name).join("card.md");
            if let Some(card) = read_card_frontmatter(&card_path) {
                let display_name = display_name_of(&card.description, &name);
                let mut names = vec![display_name.clone()];
                names.extend(card.aliases);
                index.push(FactsPeopleIndexEntry {
                    slug: name,
                    display_name,
                    names,
                });
            }
        }
    }

    index
}

pub fn known_people(entries: &[FactsPeopleIndexEntry]) -> Vec<KnownPerson> {
    entries
        .iter()
        .map(|entry| KnownPerson {
            slug: entry.slug.clone(),
            display_name: entry.display_name.clone(),
            names: entry.names.clone(),
        })
        .collect()
}

fn strip_person_prefix(s: &str) -> Option<&str> {
    let head = s.get(..6)?;
    if !head.eq_ignore_ascii_case("person") {
        return None;
    }
    s[6..].trim_start().strip_prefix('-').map(str::trim_start)
}

fn read_card_frontmatter(path: &Path) -> Option<CardFrontmatter> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_frontmatter(&raw)
}

/// The two frontmatter keys the index needs, or `None` when the file has no frontmatter or no
/// `description`.
fn parse_frontmatter(content: &str) -> Option<CardFrontmatter> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_start = &trimmed[3..];
    let end_idx = after_start.find("\n---")?;
    let fm_text = &after_start[..end_idx];

    let mut description: Option<String> = None;
    let mut aliases: Vec<String> = Vec::new();

    for line in fm_text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "description" {
            if !value.is_empty() {
                description = Some(value.to_string());
            }
        } else if key == "aliases"
            && value.starts_with('[')
            && let Ok(parsed) = serde_json::from_str::<Vec<String>>(value)
        {
            aliases = parsed
                .into_iter()
                .filter(|alias| !alias.trim().is_empty())
                .collect();
        }
    }

    Some(CardFrontmatter {
        description: description?,
        aliases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn repo(cards: &[(&str, &str)]) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        for (path, body) in cards {
            let full = dir.join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(full, body).expect("write");
        }
        dir
    }

    fn card(description: &str) -> String {
        format!("---\ndescription: {description}\n---\n\nbody\n")
    }

    #[test]
    fn the_human_card_is_read_first() {
        let dir = repo(&[("system/human.md", &card("Person - Indosaram"))]);
        let index = read_facts_people_index(&dir);
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].slug, "human");
        assert_eq!(index[0].display_name, "Indosaram");
    }

    #[test]
    fn the_person_prefix_is_stripped_from_the_display_name() {
        assert_eq!(
            display_name_of("Person - Ada Lovelace", "x"),
            "Ada Lovelace"
        );
        assert_eq!(display_name_of("person-Ada", "x"), "Ada");
        assert_eq!(display_name_of("Ada", "x"), "Ada");
    }

    #[test]
    fn a_description_that_is_only_the_prefix_falls_back_to_the_slug() {
        assert_eq!(display_name_of("Person - ", "fallback"), "fallback");
        assert_eq!(display_name_of("   ", "fallback"), "fallback");
    }

    #[test]
    fn a_card_without_a_description_is_skipped() {
        let dir = repo(&[
            ("system/human.md", "no frontmatter here"),
            ("people/ada/card.md", "---\naliases: [\"A\"]\n---\n"),
            ("people/bob/card.md", &card("Bob")),
        ]);
        let index = read_facts_people_index(&dir);
        assert_eq!(index.len(), 1, "only bob has a description");
        assert_eq!(index[0].slug, "bob");
    }

    #[test]
    fn aliases_join_the_name_list() {
        let dir = repo(&[(
            "people/ada/card.md",
            "---\ndescription: Ada\naliases: [\"A\", \"Countess\"]\n---\n",
        )]);
        let index = read_facts_people_index(&dir);
        assert_eq!(index[0].names, vec!["Ada", "A", "Countess"]);
    }

    #[test]
    fn people_are_returned_in_name_order_not_directory_order() {
        let dir = repo(&[
            ("people/zoe/card.md", &card("Zoe")),
            ("people/ada/card.md", &card("Ada")),
        ]);
        let index = read_facts_people_index(&dir);
        let slugs: Vec<&str> = index.iter().map(|entry| entry.slug.as_str()).collect();
        assert_eq!(slugs, vec!["ada", "zoe"]);
    }

    #[test]
    fn a_people_directory_named_human_is_not_read_twice() {
        let dir = repo(&[
            ("system/human.md", &card("Person - Me")),
            ("people/human/card.md", &card("Duplicate")),
        ]);
        let index = read_facts_people_index(&dir);
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].display_name, "Me");
    }

    #[test]
    fn a_missing_repository_reads_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_facts_people_index(dir.path()).is_empty());
    }

    #[test]
    fn the_index_routes_a_claim_through_the_real_routing_path() {
        let dir = repo(&[
            ("system/human.md", &card("Person - Indosaram")),
            (
                "people/indo/card.md",
                "---\ndescription: Indo\naliases: [\"Countess\"]\n---\n",
            ),
        ]);
        let entries = read_facts_people_index(&dir);
        let people = known_people(&entries);
        let claim = crate::facts_person_routing::PersonClaim {
            name: "Countess".to_string(),
            aliases: Vec::new(),
        };
        let resolved = crate::facts_person_routing::resolve_person_slug(&people, &claim, None)
            .expect("routes");
        assert_eq!(resolved, "indo");
    }

    #[test]
    fn one_name_on_two_cards_resolves_by_slug_order_and_reports_the_tie() {
        // Both cards claim the same spelling, so longest-name cannot decide it. The rule is the smallest
        // slug, and the callback is how a caller finds out the corpus has a genuinely ambiguous name.
        let dir = repo(&[
            ("system/human.md", &card("Person - Indosaram")),
            (
                "people/indo/card.md",
                "---\ndescription: Indo\naliases: [\"Indosaram\"]\n---\n",
            ),
        ]);
        let entries = read_facts_people_index(&dir);
        let people = known_people(&entries);
        let claim = crate::facts_person_routing::PersonClaim {
            name: "Indosaram".to_string(),
            aliases: Vec::new(),
        };
        let mut ties = Vec::new();
        let resolved = crate::facts_person_routing::resolve_person_slug(
            &people,
            &claim,
            Some(&mut |tie| ties.push(tie.chosen.clone())),
        )
        .expect("routes");
        assert_eq!(resolved, "human");
        assert_eq!(ties, vec!["human".to_string()]);
    }
}
