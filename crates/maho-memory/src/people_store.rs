//! Writing people records: appending observations without losing provenance.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-facts-and-people.md`. A write appends one
//! observation to one section of one person, never rewrites an existing entry, and never upgrades
//! provenance: an entry recorded from an inference stays an inference however many times it is
//! re-read. The book is pure data — callers read files in and write files out — so the same input
//! always produces the same bytes.
use std::collections::BTreeMap;

use crate::records::{OBSERVATION_SECTIONS, Observation, Person, PersonCard};
use crate::reflection::DreamPolicy;

/// The file names inside one person directory.
pub const CARD_FILE: &str = "card.md";
/// The observations file name.
pub const OBSERVATIONS_FILE: &str = "observations.md";

/// Why an observation was not written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteRefusal {
    /// The person is not known to the book.
    UnknownPerson {
        /// The slug that was asked for.
        slug: String,
    },
    /// The section name is not one of the three observation sections.
    UnknownSection {
        /// The section that was asked for.
        name: String,
    },
    /// The observation text is empty.
    Empty,
    /// The dream policy does not admit this observation.
    PolicyRefused,
}

/// What one write did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// A new entry was appended.
    Appended {
        /// The section it landed in.
        section: String,
    },
    /// The entry was already there and nothing changed.
    Unchanged {
        /// The section the existing entry lives in.
        section: String,
    },
}

/// Every person in one memory store, addressable by slug.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeopleBook {
    people: BTreeMap<String, Person>,
}

impl PeopleBook {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a book from `(slug, card, observations)` triples.
    pub fn from_records(records: impl IntoIterator<Item = (String, String, String)>) -> Self {
        let mut people = BTreeMap::new();
        for (slug, card, observations) in records {
            people.insert(slug.clone(), Person::parse(slug, &card, &observations));
        }
        Self { people }
    }

    /// How many people the book holds.
    pub fn len(&self) -> usize {
        self.people.len()
    }

    /// Whether the book holds nobody.
    pub fn is_empty(&self) -> bool {
        self.people.is_empty()
    }

    /// The slugs in order.
    pub fn slugs(&self) -> Vec<&str> {
        self.people.keys().map(String::as_str).collect()
    }

    /// One person.
    pub fn get(&self, slug: &str) -> Option<&Person> {
        self.people.get(slug)
    }

    /// Find a person by any of their names or aliases, case-insensitively.
    pub fn find_by_name(&self, name: &str) -> Option<&Person> {
        self.people.values().find(|person| {
            person
                .names()
                .iter()
                .any(|known| known.eq_ignore_ascii_case(name))
                || person.slug.eq_ignore_ascii_case(name)
        })
    }

    /// Add or replace a person.
    pub fn insert(&mut self, person: Person) {
        self.people.insert(person.slug.clone(), person);
    }

    /// Append one observation to one section of one person.
    ///
    /// A duplicate is reported as `Unchanged` rather than appended twice, and an observation the
    /// dream policy refuses is never written.
    pub fn append_observation(
        &mut self,
        slug: &str,
        section: &str,
        observation: Observation,
        policy: &DreamPolicy,
    ) -> Result<WriteOutcome, WriteRefusal> {
        let canonical = canonical_section(section).ok_or_else(|| WriteRefusal::UnknownSection {
            name: section.to_owned(),
        })?;
        if observation.text.trim().is_empty() {
            return Err(WriteRefusal::Empty);
        }
        let Some(person) = self.people.get_mut(slug) else {
            return Err(WriteRefusal::UnknownPerson {
                slug: slug.to_owned(),
            });
        };
        if let Some((existing, _)) = person.observations.sections.iter().find_map(|known| {
            known
                .entries
                .iter()
                .position(|entry| entry.text == observation.text)
                .map(|index| (known.name.clone(), index))
        }) {
            return Ok(WriteOutcome::Unchanged { section: existing });
        }
        let admitted = policy.admit([observation.text.clone()]);
        if admitted.is_empty() {
            return Err(WriteRefusal::PolicyRefused);
        }

        let entry = Observation {
            date: observation.date,
            text: admitted[0].clone(),
            src: observation.src,
            pattern: observation.pattern,
            confidence: observation.confidence,
        };
        if let Some(existing) = person
            .observations
            .sections
            .iter_mut()
            .find(|known| known.name == canonical)
        {
            existing.entries.push(entry);
        } else {
            person
                .observations
                .sections
                .push(crate::records::ObservationSection {
                    name: canonical.clone(),
                    entries: vec![entry],
                });
        }
        Ok(WriteOutcome::Appended { section: canonical })
    }

    /// The files to write for every person, as `(path, content)` pairs.
    ///
    /// Paths are `people/<slug>/card.md` and `people/<slug>/observations.md`, relative to the store
    /// root. Rendering an unmodified book reproduces the input files byte for byte.
    pub fn render_files(&self) -> Vec<(String, String)> {
        let mut files = Vec::with_capacity(self.people.len() * 2);
        for (slug, person) in &self.people {
            files.push((
                format!("people/{slug}/{CARD_FILE}"),
                render_card(&person.card),
            ));
            files.push((
                format!("people/{slug}/{OBSERVATIONS_FILE}"),
                person.observations.render(),
            ));
        }
        files
    }
}

fn canonical_section(name: &str) -> Option<String> {
    OBSERVATION_SECTIONS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(name.trim()))
        .map(|known| (*known).to_owned())
}

fn render_card(card: &PersonCard) -> String {
    let mut fields = Vec::new();
    if let Some(description) = &card.description {
        fields.push(("description".to_owned(), description.clone()));
    }
    if let Some(kind) = &card.kind {
        fields.push(("kind".to_owned(), kind.clone()));
    }
    if let Some(aliases) = &card.aliases {
        fields.push(("aliases".to_owned(), aliases.clone()));
    }
    if fields.is_empty() {
        return card.body.clone();
    }
    let mut out = String::from("---\n");
    for (key, value) in fields {
        out.push_str(&format!("{key}: {value}\n"));
    }
    out.push_str("---\n");
    out.push_str(&card.body);
    out
}

/// The canonical observation sections, for callers that validate input.
pub fn sections() -> &'static [&'static str] {
    &OBSERVATION_SECTIONS
}

/// Parse a whole store's people files into a book.
///
/// Files are `(path, content)`; only `people/<slug>/card.md` and
/// `people/<slug>/observations.md` are read, and a person missing one of the two is still read with
/// an empty file for the missing half.
pub fn book_from_files(files: impl IntoIterator<Item = (String, String)>) -> PeopleBook {
    let mut cards: BTreeMap<String, String> = BTreeMap::new();
    let mut observations: BTreeMap<String, String> = BTreeMap::new();
    for (path, content) in files {
        let Some(rest) = path.strip_prefix("people/") else {
            continue;
        };
        let Some((slug, name)) = rest.split_once('/') else {
            continue;
        };
        match name {
            CARD_FILE => {
                cards.insert(slug.to_owned(), content);
            }
            OBSERVATIONS_FILE => {
                observations.insert(slug.to_owned(), content);
            }
            _ => {}
        }
    }
    let mut slugs: Vec<String> = cards.keys().chain(observations.keys()).cloned().collect();
    slugs.sort();
    slugs.dedup();
    PeopleBook::from_records(slugs.into_iter().map(|slug| {
        let card = cards.remove(&slug).unwrap_or_default();
        let observations = observations.remove(&slug).unwrap_or_default();
        (slug, card, observations)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str =
        "---\ndescription: Person - Indosaram\nkind: person\naliases: [\"LinKeninDev\"]\n---\n";
    const OBSERVATIONS: &str = "---\ndescription: Observations - Indosaram\n---\n## Explicit\n\n- [2026-09-24] Authored commits b3340c8. <!-- src: 01a0c327; pattern: preference; confidence: high -->\n";

    fn book() -> PeopleBook {
        PeopleBook::from_records([(
            "indosaram".to_owned(),
            CARD.to_owned(),
            OBSERVATIONS.to_owned(),
        )])
    }

    fn observation(text: &str) -> Observation {
        Observation {
            date: "2026-11-10".to_owned(),
            text: text.to_owned(),
            src: Some("01a1c486".to_owned()),
            pattern: Some("preference".to_owned()),
            confidence: Some("high".to_owned()),
        }
    }

    #[test]
    fn an_unmodified_book_renders_its_files_byte_for_byte() {
        let files = book().render_files();
        assert_eq!(files[0].0, "people/indosaram/card.md");
        assert_eq!(files[0].1, CARD);
        assert_eq!(files[1].0, "people/indosaram/observations.md");
        assert_eq!(files[1].1, OBSERVATIONS);
    }

    #[test]
    fn a_store_round_trips_through_files() {
        let original = book();
        let reloaded = book_from_files(original.render_files());
        assert_eq!(reloaded, original);
    }

    #[test]
    fn appending_an_observation_keeps_every_existing_entry() {
        let mut book = book();
        let outcome = book
            .append_observation(
                "indosaram",
                "Explicit",
                observation("Prefers Korean responses."),
                &DreamPolicy::default(),
            )
            .expect("append");
        assert_eq!(
            outcome,
            WriteOutcome::Appended {
                section: "Explicit".to_owned()
            }
        );
        let rendered = book.get("indosaram").expect("person").observations.render();
        assert!(rendered.contains("Authored commits b3340c8."));
        assert!(rendered.contains("Prefers Korean responses."));
        assert!(rendered.contains("<!-- src: 01a1c486; pattern: preference; confidence: high -->"));
    }

    #[test]
    fn an_existing_entry_is_not_appended_twice() {
        let mut book = book();
        let first = book
            .append_observation(
                "indosaram",
                "Explicit",
                observation("Prefers Korean responses."),
                &DreamPolicy::default(),
            )
            .expect("append");
        assert!(matches!(first, WriteOutcome::Appended { .. }));
        let second = book
            .append_observation(
                "indosaram",
                "Explicit",
                observation("Prefers Korean responses."),
                &DreamPolicy::default(),
            )
            .expect("append");
        assert_eq!(
            second,
            WriteOutcome::Unchanged {
                section: "Explicit".to_owned()
            }
        );
        assert_eq!(
            book.get("indosaram")
                .expect("person")
                .observations
                .entries()
                .count(),
            2
        );
    }

    #[test]
    fn a_duplicate_in_another_section_reports_that_section() {
        let mut book = book();
        book.append_observation(
            "indosaram",
            "Inductive",
            observation("Prefers Korean responses."),
            &DreamPolicy::default(),
        )
        .expect("append");
        let outcome = book
            .append_observation(
                "indosaram",
                "Explicit",
                observation("Prefers Korean responses."),
                &DreamPolicy::default(),
            )
            .expect("append");
        assert_eq!(
            outcome,
            WriteOutcome::Unchanged {
                section: "Inductive".to_owned()
            }
        );
    }

    #[test]
    fn a_section_name_is_matched_case_insensitively_and_spelled_canonically() {
        let mut book = book();
        let outcome = book
            .append_observation(
                "indosaram",
                "deductive",
                observation("Infers the release cadence."),
                &DreamPolicy::default(),
            )
            .expect("append");
        assert_eq!(
            outcome,
            WriteOutcome::Appended {
                section: "Deductive".to_owned()
            }
        );
    }

    #[test]
    fn an_unknown_section_is_refused() {
        let mut book = book();
        assert_eq!(
            book.append_observation(
                "indosaram",
                "Speculation",
                observation("Something."),
                &DreamPolicy::default()
            ),
            Err(WriteRefusal::UnknownSection {
                name: "Speculation".to_owned()
            })
        );
    }

    #[test]
    fn an_unknown_person_is_refused() {
        let mut book = book();
        assert_eq!(
            book.append_observation(
                "nobody",
                "Explicit",
                observation("Something."),
                &DreamPolicy::default()
            ),
            Err(WriteRefusal::UnknownPerson {
                slug: "nobody".to_owned()
            })
        );
    }

    #[test]
    fn an_empty_observation_is_refused() {
        let mut book = book();
        assert_eq!(
            book.append_observation(
                "indosaram",
                "Explicit",
                observation("   "),
                &DreamPolicy::default()
            ),
            Err(WriteRefusal::Empty)
        );
    }

    #[test]
    fn a_disabled_people_policy_refuses_every_write() {
        let mut policy = DreamPolicy::default();
        policy.people.enabled = false;
        let mut book = book();
        assert_eq!(
            book.append_observation(
                "indosaram",
                "Explicit",
                observation("Prefers Korean responses."),
                &policy
            ),
            Err(WriteRefusal::PolicyRefused)
        );
    }

    #[test]
    fn an_over_long_observation_is_refused_rather_than_truncated() {
        let mut policy = DreamPolicy::default();
        policy.people.max_entry_chars = 10;
        let mut book = book();
        assert_eq!(
            book.append_observation(
                "indosaram",
                "Explicit",
                observation("This sentence is far longer than ten characters."),
                &policy
            ),
            Err(WriteRefusal::PolicyRefused)
        );
    }

    #[test]
    fn provenance_is_never_upgraded_by_a_later_read() {
        let mut book = book();
        book.append_observation(
            "indosaram",
            "Inductive",
            Observation {
                date: "2026-11-10".to_owned(),
                text: "Probably prefers short answers.".to_owned(),
                src: Some("agent suggestion".to_owned()),
                pattern: Some("style".to_owned()),
                confidence: Some("low".to_owned()),
            },
            &DreamPolicy::default(),
        )
        .expect("append");
        let rendered = book.get("indosaram").expect("person").observations.render();
        assert!(rendered.contains("confidence: low"));
        let reloaded = book_from_files(book.render_files());
        let entry = reloaded
            .get("indosaram")
            .expect("person")
            .observations
            .section("Inductive")
            .expect("inductive")
            .entries
            .first()
            .expect("entry")
            .clone();
        assert_eq!(entry.confidence.as_deref(), Some("low"));
        assert_eq!(entry.src.as_deref(), Some("agent suggestion"));
    }

    #[test]
    fn a_person_can_be_found_by_alias_or_slug() {
        let book = book();
        assert_eq!(
            book.find_by_name("linkenindev")
                .map(|person| person.slug.as_str()),
            Some("indosaram")
        );
        assert_eq!(
            book.find_by_name("Indosaram")
                .map(|person| person.slug.as_str()),
            Some("indosaram")
        );
        assert!(book.find_by_name("someone else").is_none());
    }

    #[test]
    fn a_person_with_only_one_file_still_loads() {
        let book = book_from_files([(
            "people/indo/card.md".to_owned(),
            "---\ndescription: Person - Indo\nkind: person\n---\n".to_owned(),
        )]);
        assert_eq!(book.slugs(), ["indo"]);
        assert_eq!(
            book.get("indo")
                .expect("person")
                .observations
                .entries()
                .count(),
            0
        );
    }

    #[test]
    fn files_outside_the_people_tree_are_ignored() {
        let book = book_from_files([
            ("system/human.md".to_owned(), "ignored".to_owned()),
            ("people/indo/notes.txt".to_owned(), "ignored".to_owned()),
        ]);
        assert!(book.is_empty());
    }

    #[test]
    fn the_three_sections_are_the_only_writable_ones() {
        assert_eq!(sections(), ["Explicit", "Inductive", "Deductive"]);
    }
}
