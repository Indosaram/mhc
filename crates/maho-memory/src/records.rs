//! Dated facts and people records from the legacy Markdown store.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-facts-and-people.md`. Facts live in
//! `notes/facts/<YYYY-MM>.md`, people in `people/<slug>/{card,observations}.md`. Both keep their
//! provenance inline — a trailing `(source: …)` on a fact, a `<!-- src: …; pattern: …;
//! confidence: … -->` comment on an observation — and re-rendering an untouched block must
//! reproduce it byte for byte, so provenance is carried, never regenerated.
use std::fmt;

/// The three observation sections, in their fixed order.
pub const OBSERVATION_SECTIONS: [&str; 3] = ["Explicit", "Inductive", "Deductive"];

/// A record that could not be parsed without losing information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordError(pub String);

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RecordError {}

/// Split leading `---` frontmatter from a block body.
fn split_frontmatter(source: &str) -> (Vec<(String, String)>, &str) {
    let Some(rest) = source.strip_prefix("---\n") else {
        return (Vec::new(), source);
    };
    let Some((front, body)) = rest.split_once("\n---\n") else {
        return (Vec::new(), source);
    };
    let mut fields = Vec::new();
    for line in front.lines() {
        if let Some((key, value)) = line.split_once(':') {
            fields.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    (fields, body)
}

fn frontmatter_value(fields: &[(String, String)], key: &str) -> Option<String> {
    fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

fn render_frontmatter(fields: &[(String, String)]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let mut out = String::from("---\n");
    for (key, value) in fields {
        out.push_str(&format!("{key}: {value}\n"));
    }
    out.push_str("---\n");
    out
}

/// One dated fact line: `- [YYYY-MM-DD] <text> (source: <provenance>)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// When the fact was learned.
    pub date: String,
    /// The fact, without the date prefix and without the provenance parenthetical.
    pub text: String,
    /// The trailing `(source: …)` body, when present. Carried verbatim.
    pub source: Option<String>,
}

impl Fact {
    /// Parse one `- [date] text (source: …)` line.
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.strip_prefix("- [")?;
        let (date, rest) = rest.split_once("] ")?;
        if date.len() != 10 || date.as_bytes().get(4) != Some(&b'-') {
            return None;
        }
        let (text, source) = match rest.rfind("(source: ") {
            Some(index) if rest.ends_with(')') => (
                rest[..index].trim_end(),
                Some(rest[index + "(source: ".len()..rest.len() - 1].to_owned()),
            ),
            _ => (rest.trim_end(), None),
        };
        if text.is_empty() {
            return None;
        }
        Some(Self {
            date: date.to_owned(),
            text: text.to_owned(),
            source,
        })
    }

    /// Render the line back, restoring the provenance parenthetical exactly.
    pub fn render(&self) -> String {
        match &self.source {
            Some(source) => format!("- [{}] {} (source: {source})\n", self.date, self.text),
            None => format!("- [{}] {}\n", self.date, self.text),
        }
    }

    /// Whether the provenance marks this as something the person never confirmed.
    pub fn is_unconfirmed(&self) -> bool {
        self.source
            .as_deref()
            .is_some_and(|source| source.contains("not a user decision"))
    }
}

/// A `notes/facts/<YYYY-MM>.md` block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FactFile {
    /// The block description.
    pub description: Option<String>,
    /// Facts in file order (newest first in every observed store).
    pub facts: Vec<Fact>,
    /// Lines that are not facts, preserved so a rewrite does not drop them.
    pub trailing: Vec<String>,
}

impl FactFile {
    /// Parse a whole facts block.
    pub fn parse(source: &str) -> Self {
        let (fields, body) = split_frontmatter(source);
        let mut facts = Vec::new();
        let mut trailing = Vec::new();
        for line in body.lines() {
            match Fact::parse(line) {
                Some(fact) => facts.push(fact),
                None if line.trim().is_empty() => {}
                None => trailing.push(line.to_owned()),
            }
        }
        Self {
            description: frontmatter_value(&fields, "description"),
            facts,
            trailing,
        }
    }

    /// The month bucket this file belongs in, from its name (`notes/facts/2026-10.md`).
    pub fn month_of(path: &str) -> Option<String> {
        let name = path.rsplit('/').next()?.strip_suffix(".md")?;
        let (year, month) = name.split_once('-')?;
        (year.len() == 4
            && month.len() == 2
            && month.parse::<u8>().is_ok_and(|m| (1..=12).contains(&m)))
        .then(|| name.to_owned())
    }

    /// Insert a fact newest-first, which is how every observed file is ordered.
    pub fn insert(&mut self, fact: Fact) {
        let position = self
            .facts
            .iter()
            .position(|existing| existing.date < fact.date)
            .unwrap_or(self.facts.len());
        self.facts.insert(position, fact);
    }

    /// Render back to Markdown; an untouched block round-trips byte for byte.
    pub fn render(&self) -> String {
        let mut fields = Vec::new();
        if let Some(description) = &self.description {
            fields.push(("description".to_owned(), description.clone()));
        }
        let mut out = render_frontmatter(&fields);
        for fact in &self.facts {
            out.push_str(&fact.render());
        }
        for line in &self.trailing {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// A person's `card.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonCard {
    /// `description:` frontmatter.
    pub description: Option<String>,
    /// `kind:` frontmatter; `person` in every observed card.
    pub kind: Option<String>,
    /// `aliases:` frontmatter, unparsed (`[]` or a JSON array).
    pub aliases: Option<String>,
    /// The card body; empty in every observed card.
    pub body: String,
}

impl PersonCard {
    /// Parse a whole card.
    pub fn parse(source: &str) -> Self {
        let (fields, body) = split_frontmatter(source);
        Self {
            description: frontmatter_value(&fields, "description"),
            kind: frontmatter_value(&fields, "kind"),
            aliases: frontmatter_value(&fields, "aliases"),
            body: body.to_owned(),
        }
    }

    /// Whether this directory is a person rather than a topic.
    pub fn is_person(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("person"))
    }

    /// Alias names, parsed from the JSON array; `[]` and malformed values yield none.
    pub fn alias_list(&self) -> Vec<String> {
        self.aliases
            .as_deref()
            .and_then(|aliases| serde_json::from_str::<Vec<String>>(aliases).ok())
            .unwrap_or_default()
    }
}

/// One observation entry with its provenance comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// When it was observed.
    pub date: String,
    /// The observation text.
    pub text: String,
    /// `src:` from the provenance comment, when present.
    pub src: Option<String>,
    /// `pattern:` from the provenance comment, when present.
    pub pattern: Option<String>,
    /// `confidence:` from the provenance comment, when present.
    pub confidence: Option<String>,
}

impl Observation {
    /// Parse `- [date] text <!-- src: …; pattern: …; confidence: … -->`.
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.strip_prefix("- [")?;
        let (date, rest) = rest.split_once("] ")?;
        if date.len() != 10 || date.as_bytes().get(4) != Some(&b'-') {
            return None;
        }
        let (text, meta) = match rest.split_once("<!--") {
            Some((text, meta)) => (text.trim_end(), Some(meta)),
            None => (rest.trim_end(), None),
        };
        if text.is_empty() {
            return None;
        }
        let mut src = None;
        let mut pattern = None;
        let mut confidence = None;
        if let Some(meta) = meta {
            let meta = meta.strip_suffix("-->").unwrap_or(meta);
            for part in meta.split(';') {
                let Some((key, value)) = part.split_once(':') else {
                    continue;
                };
                let value = value.trim();
                if value.is_empty() {
                    continue;
                }
                match key.trim() {
                    "src" => src = Some(value.to_owned()),
                    "pattern" => pattern = Some(value.to_owned()),
                    "confidence" => confidence = Some(value.to_owned()),
                    _ => {}
                }
            }
        }
        Some(Self {
            date: date.to_owned(),
            text: text.to_owned(),
            src,
            pattern,
            confidence,
        })
    }

    /// An entry kept exactly as written because it carried no recognisable date.
    pub fn verbatim(line: &str) -> Self {
        Self {
            date: String::new(),
            text: line.to_owned(),
            src: None,
            pattern: None,
            confidence: None,
        }
    }

    /// Render the entry back, keeping every provenance field that was present.
    pub fn render(&self) -> String {
        let mut meta = Vec::new();
        if let Some(src) = &self.src {
            meta.push(format!("src: {src}"));
        }
        if let Some(pattern) = &self.pattern {
            meta.push(format!("pattern: {pattern}"));
        }
        if let Some(confidence) = &self.confidence {
            meta.push(format!("confidence: {confidence}"));
        }
        if meta.is_empty() {
            format!("- [{}] {}\n", self.date, self.text)
        } else {
            format!(
                "- [{}] {} <!-- {} -->\n",
                self.date,
                self.text,
                meta.join("; ")
            )
        }
    }
}

/// One named section of an observations file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObservationSection {
    /// `Explicit`, `Inductive`, `Deductive`, or any other heading verbatim.
    pub name: String,
    /// Entries in file order.
    pub entries: Vec<Observation>,
}

/// A person's `observations.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observations {
    /// `description:` frontmatter.
    pub description: Option<String>,
    /// Sections in file order.
    pub sections: Vec<ObservationSection>,
}

impl Observations {
    /// Parse a whole observations block.
    pub fn parse(source: &str) -> Self {
        let (fields, body) = split_frontmatter(source);
        let mut sections: Vec<ObservationSection> = Vec::new();
        for line in body.lines() {
            if let Some(name) = line.strip_prefix("## ") {
                sections.push(ObservationSection {
                    name: name.trim().to_owned(),
                    entries: Vec::new(),
                });
                continue;
            }
            if let Some(entry) = Observation::parse(line) {
                if sections.is_empty() {
                    sections.push(ObservationSection::default());
                }
                sections
                    .last_mut()
                    .expect("a section was just pushed")
                    .entries
                    .push(entry);
                continue;
            }
            if !line.trim().is_empty() {
                if sections.is_empty() {
                    sections.push(ObservationSection::default());
                }
                sections
                    .last_mut()
                    .expect("a section was just pushed")
                    .entries
                    .push(Observation::verbatim(line));
            }
        }
        Self {
            description: frontmatter_value(&fields, "description"),
            sections,
        }
    }

    /// Entries across every section, in file order.
    pub fn entries(&self) -> impl Iterator<Item = &Observation> {
        self.sections
            .iter()
            .flat_map(|section| section.entries.iter())
    }

    /// Entries in one named section.
    pub fn section(&self, name: &str) -> Option<&ObservationSection> {
        self.sections.iter().find(|section| section.name == name)
    }

    /// Render back to Markdown; an untouched block round-trips byte for byte.
    pub fn render(&self) -> String {
        let mut fields = Vec::new();
        if let Some(description) = &self.description {
            fields.push(("description".to_owned(), description.clone()));
        }
        let mut out = render_frontmatter(&fields);
        for (index, section) in self.sections.iter().enumerate() {
            if !section.name.is_empty() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(&format!("## {}\n\n", section.name));
            }
            for entry in &section.entries {
                if entry.date.is_empty() {
                    out.push_str(&entry.text);
                    out.push('\n');
                } else {
                    out.push_str(&entry.render());
                }
            }
        }
        out
    }
}

/// A whole people directory: `card.md` plus `observations.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// Directory slug (`indosaram`, `indo`, `윤인도` are all observed).
    pub slug: String,
    /// The parsed card.
    pub card: PersonCard,
    /// The parsed observations.
    pub observations: Observations,
}

impl Person {
    /// Parse one person directory from its two files.
    pub fn parse(slug: impl Into<String>, card: &str, observations: &str) -> Self {
        Self {
            slug: slug.into(),
            card: PersonCard::parse(card),
            observations: Observations::parse(observations),
        }
    }

    /// The person's names: the card description suffix plus every alias.
    pub fn names(&self) -> Vec<String> {
        let mut names = self.card.alias_list();
        if let Some(description) = &self.card.description
            && let Some(name) = description.split_once(" - ").map(|(_, name)| name.trim())
            && !name.is_empty()
            && !names.iter().any(|existing| existing == name)
        {
            names.push(name.to_owned());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACTS: &str = "---
description: Explicit facts for 2026-10
---
- [2026-10-18] A Korean-UI self-study platform was published at https://example.test. (source: agent suggestion, not a user decision)
- [2026-10-06] The assistant created a local reference note. (source: agent suggestion, not a user decision)
";

    const CARD: &str = "---
description: Person - Indosaram
kind: person
aliases: [\"LinKeninDev\"]
---
";

    const OBSERVATIONS: &str = "---
description: Observations - Indosaram
---
## Explicit

- [2026-09-24] Indosaram authored commits b3340c8 in the repository. <!-- src: 01a0c327; pattern: preference; confidence: high -->
- [2026-09-26] The person is the only human user of the client.
";

    #[test]
    fn parses_a_dated_fact_and_keeps_its_provenance() {
        let file = FactFile::parse(FACTS);
        assert_eq!(file.facts.len(), 2);
        assert_eq!(file.facts[0].date, "2026-10-18");
        assert!(file.facts[0].text.ends_with("https://example.test."));
        assert_eq!(
            file.facts[0].source.as_deref(),
            Some("agent suggestion, not a user decision")
        );
        assert!(file.facts[0].is_unconfirmed());
        assert!(!file.facts[0].text.contains("source:"));
    }

    #[test]
    fn an_unconfirmed_fact_stays_unconfirmed_through_a_round_trip() {
        let file = FactFile::parse(FACTS);
        let rendered = file.render();
        assert_eq!(rendered, FACTS);
        assert!(FactFile::parse(&rendered).facts[0].is_unconfirmed());
    }

    #[test]
    fn a_fact_without_provenance_renders_without_a_parenthetical() {
        let fact = Fact::parse("- [2026-10-01] Something plain.").expect("fact");
        assert_eq!(fact.source, None);
        assert_eq!(fact.render(), "- [2026-10-01] Something plain.\n");
    }

    #[test]
    fn a_fact_with_parentheses_in_the_text_keeps_only_the_trailing_source() {
        let fact =
            Fact::parse("- [2026-10-01] Ran cargo nextest (workspace) cleanly. (source: agent)")
                .expect("fact");
        assert_eq!(fact.text, "Ran cargo nextest (workspace) cleanly.");
        assert_eq!(fact.source.as_deref(), Some("agent"));
    }

    #[test]
    fn new_facts_are_inserted_newest_first() {
        let mut file = FactFile::parse(FACTS);
        file.insert(Fact {
            date: "2026-10-25".to_owned(),
            text: "Newest.".to_owned(),
            source: None,
        });
        assert_eq!(file.facts[0].date, "2026-10-25");
        assert_eq!(file.facts[1].date, "2026-10-18");
        assert_eq!(file.facts[2].date, "2026-10-06");
    }

    #[test]
    fn a_month_bucket_is_read_from_the_file_name() {
        assert_eq!(
            FactFile::month_of("notes/facts/2026-10.md").as_deref(),
            Some("2026-10")
        );
        assert_eq!(FactFile::month_of("notes/facts/2026-13.md"), None);
        assert_eq!(FactFile::month_of("notes/facts/2026-1.md"), None);
        assert_eq!(FactFile::month_of("notes/facts/facts.md"), None);
    }

    #[test]
    fn a_person_card_carries_kind_and_aliases() {
        let card = PersonCard::parse(CARD);
        assert!(card.is_person());
        assert_eq!(card.alias_list(), ["LinKeninDev"]);
        assert!(card.body.trim().is_empty());
    }

    #[test]
    fn an_empty_alias_array_yields_no_names() {
        let card =
            PersonCard::parse("---\ndescription: Person - X\nkind: person\naliases: []\n---\n");
        assert!(card.alias_list().is_empty());
        assert_eq!(
            PersonCard::parse("---\nkind: person\naliases: not json\n---\n").alias_list(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn observations_keep_their_section_and_provenance() {
        let parsed = Observations::parse(OBSERVATIONS);
        let explicit = parsed.section("Explicit").expect("explicit section");
        assert_eq!(explicit.entries.len(), 2);
        assert_eq!(explicit.entries[0].src.as_deref(), Some("01a0c327"));
        assert_eq!(explicit.entries[0].pattern.as_deref(), Some("preference"));
        assert_eq!(explicit.entries[0].confidence.as_deref(), Some("high"));
        assert_eq!(explicit.entries[1].src, None);
    }

    #[test]
    fn observations_round_trip_byte_for_byte() {
        let parsed = Observations::parse(OBSERVATIONS);
        assert_eq!(parsed.render(), OBSERVATIONS);
    }

    #[test]
    fn every_observation_section_is_kept_in_order() {
        let source = "---\ndescription: Observations - X\n---\n## Explicit\n\n- [2026-01-01] Stated.\n\n## Inductive\n\n- [2026-01-02] Inferred.\n\n## Deductive\n\n- [2026-01-03] Derived.\n";
        let parsed = Observations::parse(source);
        let names: Vec<_> = parsed
            .sections
            .iter()
            .map(|section| section.name.as_str())
            .collect();
        assert_eq!(names, OBSERVATION_SECTIONS);
        assert_eq!(parsed.entries().count(), 3);
        assert_eq!(parsed.render(), source);
    }

    #[test]
    fn a_person_exposes_its_card_name_and_aliases() {
        let person = Person::parse("indosaram", CARD, OBSERVATIONS);
        assert_eq!(person.names(), ["LinKeninDev", "Indosaram"]);
        assert!(person.card.is_person());
        assert_eq!(person.observations.entries().count(), 2);
    }
}
