//! Reader for the people memory index across human and person cards.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Person entry in the index with slug, display name, and all known aliases.
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

/// Extract clean display name by stripping the leading "Person - " prefix.
pub fn display_name_of(description: &str, fallback: &str) -> String {
    let stripped = if let Some(rest) = strip_person_prefix(description) {
        rest.trim()
    } else {
        description.trim()
    };
    if stripped.is_empty() {
        fallback.to_string()
    } else {
        stripped.to_string()
    }
}

/// Read people index from the memory repository filesystem.
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
                .filter(|s| !s.trim().is_empty())
                .collect();
        }
    }

    let description = description?;
    Some(CardFrontmatter {
        description,
        aliases,
    })
}

#[cfg(test)]
#[path = "person_index_tests.rs"]
mod tests;
