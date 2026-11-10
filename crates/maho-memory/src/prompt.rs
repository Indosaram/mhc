//! The memory section of the system prompt.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-prompt.md`. The section is assembled from a
//! projection's injected slots, a short index of the blocks that are only listed by name, the most
//! recent facts, and the people the agent knows. Material is added in priority order and a budget
//! drops whole entries — never a truncated line — so a reader never sees half a memory.
use crate::records::{Fact, Person};

/// The system slots, in the order they are injected.
pub const SLOT_ORDER: [&str; 5] = ["persona", "identity", "human", "boundaries", "self-aware"];

/// What the prompt section may spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptBudget {
    /// Most characters the whole section may use.
    pub max_chars: usize,
    /// Most facts to consider.
    pub max_facts: usize,
    /// Most people to name.
    pub max_people: usize,
}

impl Default for PromptBudget {
    fn default() -> Self {
        Self {
            max_chars: 8_000,
            max_facts: 12,
            max_people: 8,
        }
    }
}

/// One part of the assembled section, in priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    /// A `system/` slot whose body is injected whole.
    Slot,
    /// The name-only index of blocks that are not injected.
    Index,
    /// The most recent facts.
    Facts,
    /// The people the agent knows.
    People,
}

/// Which parts survived the budget.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssembleReport {
    /// The rendered section.
    pub rendered: String,
    /// Parts that fit, in priority order.
    pub included: Vec<Part>,
    /// Parts dropped whole for the budget, with the reason.
    pub dropped: Vec<(Part, String)>,
}

/// Assemble the memory section of the system prompt.
///
/// Slots come first, then the name index, then facts and people; a part that would exceed
/// `max_chars` is dropped whole rather than truncated. Slots are ordered by [`SLOT_ORDER`] and then
/// by name, so the same store always renders the same section.
pub fn assemble(
    slots: &[(String, String)],
    names: &[String],
    facts: &[Fact],
    people: &[Person],
    budget: PromptBudget,
) -> AssembleReport {
    let mut ordered: Vec<&(String, String)> = slots.iter().collect();
    ordered.sort_by_key(|(slot, _)| {
        (
            SLOT_ORDER
                .iter()
                .position(|known| known == slot)
                .unwrap_or(SLOT_ORDER.len()),
            slot.clone(),
        )
    });

    let facts_section = render_facts(&facts[..facts.len().min(budget.max_facts)]);
    let people_section = render_people(&people[..people.len().min(budget.max_people)]);
    let index_section = if names.is_empty() {
        String::new()
    } else {
        let mut out = String::from("<external_projection>\n");
        for name in names {
            out.push_str(name);
            out.push('\n');
        }
        out.push_str("</external_projection>\n");
        out
    };

    let mut report = AssembleReport::default();
    let mut used = 0usize;
    let mut slots_rendered = 0usize;

    for (slot, body) in ordered {
        let block = format!("<{slot}>\n{}\n</{slot}>\n", body.trim_end());
        let size = block.chars().count();
        if used + size > budget.max_chars {
            report
                .dropped
                .push((Part::Slot, format!("{slot} does not fit the prompt budget")));
            continue;
        }
        used += size;
        slots_rendered += 1;
        report.rendered.push_str(&block);
    }
    if slots_rendered > 0 {
        report.included.push(Part::Slot);
    }

    for (part, section) in [
        (Part::Index, index_section),
        (Part::Facts, facts_section),
        (Part::People, people_section),
    ] {
        if section.is_empty() {
            continue;
        }
        let size = section.chars().count();
        if used + size > budget.max_chars {
            report
                .dropped
                .push((part, "does not fit the prompt budget".to_owned()));
            continue;
        }
        used += size;
        report.rendered.push_str(&section);
        report.included.push(part);
    }

    report
}

fn render_facts(facts: &[Fact]) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut out = String::from("<recent_facts>\n");
    for fact in facts {
        out.push_str("- [");
        out.push_str(&fact.date);
        out.push_str("] ");
        out.push_str(&fact.text);
        out.push('\n');
    }
    out.push_str("</recent_facts>\n");
    out
}

fn render_people(people: &[Person]) -> String {
    if people.is_empty() {
        return String::new();
    }
    let mut out = String::from("<people>\n");
    for person in people {
        let names = person.names();
        if names.is_empty() {
            out.push_str("- ");
            out.push_str(&person.slug);
        } else {
            out.push_str("- ");
            out.push_str(&names.join(", "));
        }
        out.push('\n');
    }
    out.push_str("</people>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{Person, PersonCard};

    fn slot(name: &str, body: &str) -> (String, String) {
        (name.to_owned(), body.to_owned())
    }

    fn fact(date: &str, text: &str) -> Fact {
        Fact {
            date: date.to_owned(),
            text: text.to_owned(),
            source: None,
        }
    }

    fn person(slug: &str, alias: &str) -> Person {
        let card = PersonCard::parse(&format!(
            "---\ndescription: Person - {slug}\nkind: person\naliases: [\"{alias}\"]\n---\n"
        ));
        Person {
            slug: slug.to_owned(),
            card,
            observations: Default::default(),
        }
    }

    #[test]
    fn slots_render_in_the_fixed_order_not_the_input_order() {
        let slots = vec![slot("self-aware", "Aware."), slot("persona", "Self.")];
        let report = assemble(&slots, &[], &[], &[], PromptBudget::default());
        let persona = report.rendered.find("<persona>").expect("persona");
        let aware = report.rendered.find("<self-aware>").expect("self-aware");
        assert!(persona < aware);
    }

    #[test]
    fn every_part_renders_when_the_budget_allows() {
        let report = assemble(
            &[slot("persona", "Self.")],
            &["notes/facts".to_owned()],
            &[fact("2026-11-10", "A fact.")],
            &[person("indo", "Indosaram")],
            PromptBudget::default(),
        );
        assert_eq!(
            report.included,
            [Part::Slot, Part::Index, Part::Facts, Part::People]
        );
        assert!(report.dropped.is_empty());
        assert!(report.rendered.contains("<persona>\nSelf.\n</persona>"));
        assert!(report.rendered.contains("- [2026-11-10] A fact."));
        assert!(report.rendered.contains("Indosaram, indo"));
    }

    #[test]
    fn an_empty_store_renders_nothing() {
        let report = assemble(&[], &[], &[], &[], PromptBudget::default());
        assert!(report.rendered.is_empty());
        assert!(report.included.is_empty());
        assert!(report.dropped.is_empty());
    }

    #[test]
    fn facts_beyond_the_limit_are_not_rendered() {
        let facts: Vec<_> = (0..5)
            .map(|index| fact("2026-11-10", &format!("fact {index}")))
            .collect();
        let budget = PromptBudget {
            max_facts: 2,
            ..PromptBudget::default()
        };
        let report = assemble(&[], &[], &facts, &[], budget);
        assert!(report.rendered.contains("fact 0"));
        assert!(report.rendered.contains("fact 1"));
        assert!(!report.rendered.contains("fact 2"));
    }

    #[test]
    fn a_part_that_does_not_fit_is_dropped_whole_never_truncated() {
        let budget = PromptBudget {
            max_chars: 40,
            ..PromptBudget::default()
        };
        let report = assemble(
            &[],
            &[],
            &[fact("2026-11-10", &"x".repeat(200))],
            &[],
            budget,
        );
        assert!(report.rendered.is_empty());
        assert_eq!(report.dropped.len(), 1);
        assert_eq!(report.dropped[0].0, Part::Facts);
        assert!(!report.included.contains(&Part::Facts));
    }

    #[test]
    fn a_later_cheaper_part_still_fits_after_an_expensive_one_is_dropped() {
        let budget = PromptBudget {
            max_chars: 120,
            ..PromptBudget::default()
        };
        let report = assemble(
            &[],
            &[],
            &[fact("2026-11-10", &"x".repeat(200))],
            &[person("indo", "Indosaram")],
            budget,
        );
        assert!(report.included.contains(&Part::People));
        assert!(!report.included.contains(&Part::Facts));
        assert!(report.rendered.contains("<people>"));
    }

    #[test]
    fn slots_that_do_not_fit_are_reported_individually() {
        let budget = PromptBudget {
            max_chars: 30,
            ..PromptBudget::default()
        };
        let slots = vec![slot("persona", "Self."), slot("human", &"y".repeat(200))];
        let report = assemble(&slots, &[], &[], &[], budget);
        assert!(report.rendered.contains("<persona>"));
        assert!(!report.rendered.contains("<human>"));
        assert_eq!(report.dropped.len(), 1);
        assert!(report.dropped[0].1.contains("human"));
    }

    #[test]
    fn people_without_names_fall_back_to_their_slug() {
        let card = PersonCard::parse("---\nkind: person\naliases: []\n---\n");
        let bare = Person {
            slug: "anonymous".to_owned(),
            card,
            observations: Default::default(),
        };
        let report = assemble(&[], &[], &[], &[bare], PromptBudget::default());
        assert!(report.rendered.contains("- anonymous"));
    }

    #[test]
    fn the_rendered_section_is_stable_for_the_same_input() {
        let slots = vec![slot("human", "Human."), slot("persona", "Self.")];
        let facts = vec![fact("2026-11-10", "A fact.")];
        let people = vec![person("indo", "Indosaram")];
        let names = vec!["a".to_owned()];
        let first = assemble(&slots, &names, &facts, &people, PromptBudget::default());
        let second = assemble(&slots, &names, &facts, &people, PromptBudget::default());
        assert_eq!(first.rendered, second.rendered);
    }
}
