//! Transcript evidence for `/people <name> --ask`: the same FTS-lite scan `/search`
//! runs, narrowed to the person's name and aliases.
//! Port of `components/memory/commands/people-search.ts` at pin 77f3067f1.

use std::path::PathBuf;

use memory_core::search::{
    SearchOptions, SenpiSessionProvider, SenpiSessionProviderOptions, search_transcripts,
    searchable_text,
};

use crate::palace::people::PalacePeopleNode;

use super::types::{CommandContext, MemoryCommandDeps};

const MAX_HITS: usize = 10;
const SNIPPET_CHARS: usize = 240;

pub fn collect_person_search_hits(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    node: &PalacePeopleNode,
) -> Vec<String> {
    let Some(sessions_dir) = resolve_sessions_dir(deps, ctx).filter(|dir| dir.exists()) else {
        return Vec::new();
    };

    let needles: Vec<String> = std::iter::once(node.display_name.clone())
        .chain(node.aliases.iter().cloned())
        .map(|needle| needle.trim().to_owned())
        .filter(|needle| !needle.is_empty())
        .collect();
    if needles.is_empty() {
        return Vec::new();
    }

    let provider = SenpiSessionProvider::new(SenpiSessionProviderOptions {
        sessions_dir,
        excluded_dirs: None,
        hidden_marker_file: None,
        is_hidden: None,
    });
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut hits: Vec<String> = Vec::new();
    for needle in &needles {
        let query = format!("\"{needle}\"");
        for result in search_transcripts(&provider, &query, &SearchOptions::default()) {
            if hits.len() >= MAX_HITS {
                return hits;
            }
            if !seen.insert(result.message_id.clone()) {
                continue;
            }
            hits.push(format!(
                "{} {}",
                result.created_at,
                snippet_of(&searchable_text(&result.document))
            ));
        }
    }
    hits
}

fn snippet_of(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SNIPPET_CHARS {
        collapsed
    } else {
        format!("{}...", collapsed.chars().take(SNIPPET_CHARS).collect::<String>())
    }
}

fn resolve_sessions_dir(deps: &MemoryCommandDeps, ctx: &CommandContext) -> Option<PathBuf> {
    deps.sessions_root(ctx)
}
