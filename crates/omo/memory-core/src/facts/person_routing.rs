//! Person routing for facts extraction.
//!
//! Routes person-scoped records to `people/<slug>/observations.md` ledgers,
//! minting a card skeleton when the person is new and confidently named.
//! Alias matching is case-insensitive, longest-alias-wins, with a
//! lexicographic slug tie-break that is reported. Reinforcement equality is
//! NFKC textual, never semantic. The whole path is gated by `people.enabled`.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use super::extraction::{FactsExtractionRecord, FactsPersonReference};
use super::person_index::{FactsPeopleIndexEntry, read_facts_people_index};
use crate::memfs::{FrontmatterError, MemoryFrontmatter, parse_memory_file, render_memory_file};
use crate::people::{
    ObservationEntry, ObservationGroup, PeopleCard, PeopleLimits, parse_people_card,
    resolve_slug_collision, sanitize_person_slug, serialize_people_card,
};

/// Routing policy parameters for people observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPeopleRouting {
    pub enabled: bool,
    #[serde(rename = "maxEntries")]
    pub max_entries: usize,
    #[serde(rename = "maxEntryChars")]
    pub max_entry_chars: usize,
}

impl FactsPeopleRouting {
    /// The people-card limits carried by this routing policy.
    pub fn limits(&self) -> PeopleLimits {
        PeopleLimits {
            max_entries: self.max_entries,
            max_entry_chars: self.max_entry_chars,
        }
    }
}

/// Information about an alias collision tie-break between multiple slugs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsAliasTie {
    pub alias: String,
    pub slugs: Vec<String>,
    pub chosen: String,
}

/// Target person card and observation ledger destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsPersonTarget {
    pub slug: String,
    pub display_name: String,
    pub is_new: bool,
    pub person: Option<FactsPersonReference>,
}

/// Observation records grouped for a specific person target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationBucket {
    pub target: FactsPersonTarget,
    pub records: Vec<FactsExtractionRecord>,
}

/// Plan for routing extracted records into notes and person observations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsRoutingPlan {
    pub notes: BTreeMap<String, Vec<FactsExtractionRecord>>,
    pub observations: BTreeMap<String, ObservationBucket>,
}

/// Callback invoked when two slugs tie on the longest matching alias.
pub type AliasTieCallback<'a> = &'a mut dyn FnMut(&FactsAliasTie);

/// Route every record to a monthly notes file or a person ledger.
pub fn plan_facts_routing(
    repo_dir: &Path,
    records: &[FactsExtractionRecord],
    people: Option<&FactsPeopleRouting>,
    mut on_alias_tie: Option<&mut dyn FnMut(&FactsAliasTie)>,
) -> FactsRoutingPlan {
    let mut plan = FactsRoutingPlan::default();
    let add_note = |plan: &mut FactsRoutingPlan, record: &FactsExtractionRecord| {
        let month: String = record.date().chars().take(7).collect();
        plan.notes
            .entry(format!("notes/facts/{month}.md"))
            .or_default()
            .push(record.clone());
    };
    if !people.is_some_and(|policy| policy.enabled) {
        for record in records {
            add_note(&mut plan, record);
        }
        return plan;
    }

    let mut index = read_facts_people_index(repo_dir);
    let mut taken: HashSet<String> = index.iter().map(|entry| entry.slug.clone()).collect();
    for record in records {
        let person = match record {
            FactsExtractionRecord::Project { .. } => {
                add_note(&mut plan, record);
                continue;
            }
            FactsExtractionRecord::Person { person, .. } => person,
        };
        let slug = match resolve_person_slug(
            &index,
            person,
            on_alias_tie
                .as_mut()
                .map(|cb| &mut **cb as AliasTieCallback<'_>),
        ) {
            Some(slug) => slug,
            None => {
                let slug = resolve_slug_collision(&sanitize_person_slug(&person.name), &taken);
                taken.insert(slug.clone());
                let mut names = vec![person.name.clone()];
                names.extend(person.aliases.iter().cloned());
                index.push(FactsPeopleIndexEntry {
                    slug: slug.clone(),
                    display_name: person.name.clone(),
                    names,
                });
                plan.observations.insert(
                    slug.clone(),
                    ObservationBucket {
                        target: FactsPersonTarget {
                            slug: slug.clone(),
                            display_name: person.name.clone(),
                            is_new: true,
                            person: Some(person.clone()),
                        },
                        records: Vec::new(),
                    },
                );
                slug
            }
        };
        let bucket = plan.observations.entry(slug.clone()).or_insert_with(|| {
            let display_name = index
                .iter()
                .find(|entry| entry.slug == slug)
                .map_or_else(|| slug.clone(), |entry| entry.display_name.clone());
            ObservationBucket {
                target: FactsPersonTarget {
                    slug: slug.clone(),
                    display_name,
                    is_new: false,
                    person: None,
                },
                records: Vec::new(),
            }
        });
        bucket.records.push(record.clone());
    }
    plan
}

/// Every repository path the routing plan will write, sorted.
pub fn facts_routing_paths(plan: &FactsRoutingPlan) -> Vec<String> {
    let mut paths: Vec<String> = plan.notes.keys().cloned().collect();
    for (slug, bucket) in &plan.observations {
        paths.push(format!("people/{slug}/observations.md"));
        if bucket.target.is_new {
            paths.push(format!("people/{slug}/card.md"));
        }
    }
    paths.sort();
    paths
}

/// Render card and observations files for all person targets in the routing plan.
pub fn render_person_targets(
    repo_dir: &Path,
    plan: &FactsRoutingPlan,
    limits: PeopleLimits,
) -> Result<BTreeMap<String, String>, FrontmatterError> {
    let mut content = BTreeMap::new();
    for (slug, bucket) in &plan.observations {
        if bucket.target.is_new {
            content.insert(
                format!("people/{slug}/card.md"),
                render_card_skeleton(&bucket.target)?,
            );
        }
        content.insert(
            format!("people/{slug}/observations.md"),
            render_explicit_observations(
                repo_dir,
                slug,
                &bucket.target.display_name,
                &bucket.records,
                limits,
            )?,
        );
    }
    Ok(content)
}

/// NFKC-fold, lowercase, collapse whitespace, and strip a trailing comment and punctuation.
pub fn normalize_observation_text(text: &str) -> String {
    let folded: String = text.nfkc().collect::<String>().to_lowercase();
    let collapsed = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    let uncommented = strip_trailing_comment(&collapsed).trim();
    uncommented
        .trim_end_matches(is_punctuation)
        .trim()
        .to_string()
}

/// Render the frontmatter-only skeleton for a new person card.
pub fn render_card_skeleton(target: &FactsPersonTarget) -> Result<String, FrontmatterError> {
    let Some(person) = &target.person else {
        return Err(FrontmatterError {
            message: format!("New person target lacks identity: {}", target.slug),
        });
    };
    render_memory_file(
        &MemoryFrontmatter {
            description: format!("Person - {}", person.name),
            read_only: None,
            kind: Some("person".to_string()),
            aliases: Some(person.aliases.clone()),
        },
        "",
    )
}

/// Resolve a person against the index: longest matching alias wins, ties go to the smallest slug.
pub fn resolve_person_slug(
    index: &[FactsPeopleIndexEntry],
    person: &FactsPersonReference,
    on_alias_tie: Option<AliasTieCallback<'_>>,
) -> Option<String> {
    let inputs: Vec<String> = std::iter::once(&person.name)
        .chain(person.aliases.iter())
        .map(|value| fold_alias(value))
        .collect();
    let mut matches: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for entry in index {
        for name in &entry.names {
            if !inputs.contains(&fold_alias(name)) {
                continue;
            }
            let length = name.encode_utf16().count();
            let replace = matches
                .get(entry.slug.as_str())
                .is_none_or(|(current, _)| length > *current);
            if replace {
                matches.insert(entry.slug.as_str(), (length, name.as_str()));
            }
        }
    }
    let longest = matches.values().map(|(length, _)| *length).max()?;
    let winners: Vec<(&str, &str)> = matches
        .iter()
        .filter(|(_, (length, _))| *length == longest)
        .map(|(slug, (_, alias))| (*slug, *alias))
        .collect();
    let (chosen, alias) = winners[0];
    if winners.len() > 1
        && let Some(callback) = on_alias_tie
    {
        callback(&FactsAliasTie {
            alias: alias.to_string(),
            slugs: winners
                .iter()
                .map(|(slug, _)| (*slug).to_string())
                .collect(),
            chosen: chosen.to_string(),
        });
    }
    Some(chosen.to_string())
}

fn render_explicit_observations(
    repo_dir: &Path,
    slug: &str,
    display_name: &str,
    records: &[FactsExtractionRecord],
    limits: PeopleLimits,
) -> Result<String, FrontmatterError> {
    let path = repo_dir.join("people").join(slug).join("observations.md");
    let (frontmatter, body) = match std::fs::read_to_string(&path) {
        Ok(existing) => {
            let parsed = parse_memory_file(&existing)?;
            (parsed.frontmatter, parsed.body)
        }
        Err(_) => (
            MemoryFrontmatter {
                description: format!("Observations - {display_name}"),
                read_only: None,
                kind: None,
                aliases: None,
            },
            String::new(),
        ),
    };
    let card = parse_people_card(&body, limits).card;
    let mut groups: Vec<ObservationGroup> = card.observations.unwrap_or_default();
    let explicit_at = match groups.iter().position(|group| group.section == "Explicit") {
        Some(at) => at,
        None => {
            groups.insert(
                0,
                ObservationGroup {
                    section: "Explicit".to_string(),
                    entries: Vec::new(),
                },
            );
            0
        }
    };
    let explicit = &mut groups[explicit_at].entries;
    for record in records {
        let normalized = normalize_observation_text(record.text());
        match explicit
            .iter_mut()
            .find(|entry| normalize_observation_text(&entry.content) == normalized)
        {
            Some(found) => {
                found.date = record.date().to_string();
                found.n = Some(found.n.unwrap_or(1) + 1);
            }
            None => explicit.push(ObservationEntry {
                date: record.date().to_string(),
                content: record.text().to_string(),
                ..ObservationEntry::default()
            }),
        }
    }
    let rendered = serialize_people_card(
        &PeopleCard {
            entries: card.entries,
            observations: Some(groups),
        },
        limits,
    );
    render_memory_file(&frontmatter, &format!("{rendered}\n"))
}

/// Strip a single trailing `<!-- ... -->` comment, matching the TS `/<!--[\s\S]*?-->\s*$/`.
fn strip_trailing_comment(text: &str) -> &str {
    let trimmed = text.trim_end();
    if !trimmed.ends_with("-->") {
        return text;
    }
    // The lazy TS regex anchors at the earliest `<!--` whose first `-->` is the trailing one.
    let mut search_from = 0;
    while let Some(offset) = trimmed[search_from..].find("<!--") {
        let start = search_from + offset;
        let after_open = start + 4;
        if let Some(close) = trimmed[after_open..].find("-->")
            && after_open + close + 3 == trimmed.len()
        {
            return &text[..start];
        }
        search_from = after_open;
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

fn fold_alias(value: &str) -> String {
    value
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .trim()
        .to_string()
}

#[cfg(test)]
#[path = "person_routing_tests.rs"]
mod tests;
