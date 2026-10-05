//! Text rendering for /people: the roster with its adjacency tree, and one
//! person's card plus observations.
//! Port of `components/memory/commands/people-render.ts` at pin 77f3067f1.

use memory_core::{
    memfs::parse_memory_file,
    people::format::{ObservationGroup, PeopleLimits, parse_people_card},
};

use crate::palace::{PalacePeople, PalacePeopleEdge, PalacePeopleNode};

const TREE_BRANCH: &str = "|-";
const TREE_LAST: &str = "`-";

/// Card entry lines of a person file, formatted `PREFIX: content`.
pub fn parse_people_card_lines(raw: Option<&str>, limits: PeopleLimits) -> Vec<String> {
    let Some(body) = file_body(raw) else {
        return Vec::new();
    };
    parse_people_card(&body, limits)
        .card
        .entries
        .iter()
        .map(|entry| format!("{}: {}", entry.prefix, entry.content))
        .collect()
}

/// Observation groups of a person ledger, in file order.
pub fn parse_people_observations(raw: Option<&str>, limits: PeopleLimits) -> Vec<ObservationGroup> {
    let Some(body) = file_body(raw) else {
        return Vec::new();
    };
    parse_people_card(&body, limits).card.observations.unwrap_or_default()
}

pub fn render_roster(graph: &PalacePeople) -> String {
    if graph.nodes.is_empty() {
        return "No people records yet. They accrue as facts about people are saved.".to_owned();
    }
    let mut lines = vec![format!("# People ({})", graph.nodes.len()), String::new()];
    for node in &graph.nodes {
        let aliases = if node.aliases.is_empty() {
            String::new()
        } else {
            format!(" ({})", node.aliases.join(", "))
        };
        lines.push(format!("{}  {}{aliases}", node.slug, node.display_name));
        let outgoing: Vec<&PalacePeopleEdge> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == node.slug)
            .collect();
        for (index, edge) in outgoing.iter().enumerate() {
            let marker = if index + 1 == outgoing.len() { TREE_LAST } else { TREE_BRANCH };
            let unknown = if edge.target_slug.is_none() { " (no card)" } else { "" };
            lines.push(format!("  {marker} {} -> {}{unknown}", edge.predicate, edge.target));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "{} relationship{} across {} card{}.",
        graph.edges.len(),
        if graph.edges.len() == 1 { "" } else { "s" },
        graph.nodes.len(),
        if graph.nodes.len() == 1 { "" } else { "s" }
    ));
    for diagnostic in &graph.diagnostics {
        lines.push(format!("! {}: {}", diagnostic.path, diagnostic.message));
    }
    lines.join("\n")
}

pub fn render_person_card(
    node: &PalacePeopleNode,
    card_lines: &[String],
    observations: &[ObservationGroup],
    edges: &[PalacePeopleEdge],
) -> String {
    let aliases = if node.aliases.is_empty() {
        String::new()
    } else {
        format!(" | aliases: {}", node.aliases.join(", "))
    };
    let mut lines = vec![
        format!("# {} ({})", node.display_name, node.slug),
        format!("kind: {}{aliases}", node.kind),
        format!("state: {} | path: {}", node.state, node.path),
        String::new(),
        "## Card".to_owned(),
    ];
    if card_lines.is_empty() {
        lines.push("(empty card)".to_owned());
    } else {
        lines.extend(card_lines.iter().cloned());
    }

    let related: Vec<&PalacePeopleEdge> = edges
        .iter()
        .filter(|edge| {
            edge.source == node.slug
                || edge.target_slug.as_deref() == Some(node.slug.as_str())
        })
        .collect();
    if !related.is_empty() {
        lines.push(String::new());
        lines.push("## Relationships".to_owned());
        for edge in related {
            lines.push(format!("{} {} -> {}", edge.source, edge.predicate, edge.target));
        }
    }

    lines.push(String::new());
    lines.push("## Observations".to_owned());
    if observations.is_empty() {
        lines.push("(none recorded)".to_owned());
        return lines.join("\n");
    }
    for group in observations {
        lines.push(String::new());
        lines.push(format!("### {} ({})", group.section, group.entries.len()));
        for entry in &group.entries {
            lines.push(format!("- [{}] {}{}", entry.date, entry.content, suffix_of(entry)));
        }
    }
    lines.join("\n")
}

fn suffix_of(entry: &memory_core::people::format::ObservationEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = entry.n {
        parts.push(format!("n={n}"));
    }
    if let Some(pattern) = &entry.pattern {
        parts.push(format!("pattern: {pattern}"));
    }
    if let Some(confidence) = &entry.confidence {
        parts.push(format!("confidence: {confidence}"));
    }
    if let Some(status) = &entry.status {
        parts.push(format!("status: {status}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  ({})", parts.join("; "))
    }
}

fn file_body(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    parse_memory_file(raw).ok().map(|parsed| parsed.body)
}
