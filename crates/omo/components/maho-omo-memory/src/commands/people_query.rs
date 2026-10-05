//! Name resolution for `/people <name>`: exact slug, then alias or display name,
//! then a bounded "did you mean" list.
//! Port of `components/memory/commands/people-query.ts` at pin 77f3067f1.

use crate::palace::PalacePeopleNode;

const MAX_CLOSE_SLUGS: usize = 5;
const MIN_SHARED_PREFIX: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonQueryResolution {
    Hit { slug: String },
    Miss { close: Vec<String> },
}

pub fn resolve_person_query(nodes: &[PalacePeopleNode], query: &str) -> PersonQueryResolution {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return PersonQueryResolution::Miss {
            close: nodes.iter().map(|node| node.slug.clone()).collect(),
        };
    }

    if let Some(exact) = nodes
        .iter()
        .find(|node| node.slug.to_lowercase() == needle)
    {
        return PersonQueryResolution::Hit { slug: exact.slug.clone() };
    }

    if let Some(named) = nodes.iter().find(|node| {
        node.display_name.to_lowercase() == needle
            || node
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase() == needle)
    }) {
        return PersonQueryResolution::Hit { slug: named.slug.clone() };
    }

    PersonQueryResolution::Miss { close: close_slugs(nodes, &needle) }
}

fn close_slugs(nodes: &[PalacePeopleNode], needle: &str) -> Vec<String> {
    let collapsed = collapse(needle);
    let mut scored: Vec<(i64, String)> = nodes
        .iter()
        .map(|node| (closeness(node, needle, &collapsed), node.slug.clone()))
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored
        .into_iter()
        .take(MAX_CLOSE_SLUGS)
        .map(|(_, slug)| slug)
        .collect()
}

fn closeness(node: &PalacePeopleNode, needle: &str, collapsed: &str) -> i64 {
    let slug = node.slug.to_lowercase();
    let flat = collapse(&slug);
    if slug.contains(needle) || needle.contains(&slug) {
        return 3;
    }
    if flat.contains(collapsed) || collapsed.contains(&flat) {
        return 3;
    }
    let shared = shared_prefix(&flat, collapsed);
    if shared >= MIN_SHARED_PREFIX {
        let denominator = flat.len().max(collapsed.len()).max(1) as i64;
        return 1 + (shared as i64) / denominator;
    }
    0
}

fn collapse(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        .collect()
}

fn shared_prefix(left: &str, right: &str) -> usize {
    let mut index = 0;
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    while index < left_bytes.len() && index < right_bytes.len() && left_bytes[index] == right_bytes[index]
    {
        index += 1;
    }
    index
}
