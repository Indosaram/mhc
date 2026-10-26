//! Project-scoped recall seats (#10044).
//!
//! The session's own notes and each touched project's notes are ranked separately. Merging
//! them into one corpus changed the own ranking (strategy choice and term weights depend on the
//! corpus) and gave a small project no seat against thousands of own notes. Projects get up to
//! half the seats, rounded up, in touch order; seats one side cannot fill go to the other.

use std::collections::HashSet;
use memory_core::recall::{RecallCandidate, RecallDocument};

pub const PROJECT_MEMORY_PATH_PREFIX: &str = "@project/";

pub fn qualify_project_documents(identity_id: &str, documents: &[RecallDocument]) -> Vec<RecallDocument> {
    documents
        .iter()
        .map(|doc| RecallDocument {
            path: format!("{PROJECT_MEMORY_PATH_PREFIX}{identity_id}/{}", doc.path),
            description: doc.description.clone(),
            body: doc.body.clone(),
        })
        .collect()
}

pub fn parse_project_memory_path(path: &str) -> Option<(String, String)> {
    if !path.starts_with(PROJECT_MEMORY_PATH_PREFIX) {
        return None;
    }
    let rest = &path[PROJECT_MEMORY_PATH_PREFIX.len()..];
    let slash = rest.find('/')?;
    if slash == 0 || slash == rest.len() - 1 {
        return None;
    }
    Some((rest[..slash].to_string(), rest[slash + 1..].to_string()))
}

/// One touched project's store-backed notes, plus the touched file relative to its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecallCorpus {
    pub identity_id: String,
    pub documents: Vec<RecallDocument>,
    pub touched_path: Option<String>,
}

/// The session's own notes and each touched project's notes are ranked separately (#10044).
pub fn select_with_project_seats<F>(
    own: &[RecallDocument],
    projects: &[ProjectRecallCorpus],
    max_items: usize,
    rank: F,
) -> Vec<RecallCandidate>
where
    F: Fn(&[RecallDocument], &[String], Option<&str>) -> Vec<RecallCandidate>,
{
    let own_ranked = if own.is_empty() {
        Vec::new()
    } else {
        rank(own, &[], None)
    };
    if projects.is_empty() || max_items == 0 {
        return own_ranked.into_iter().take(max_items).collect();
    }
    let mut project_ranked_lists: Vec<Vec<RecallCandidate>> = Vec::new();
    for project in projects {
        if project.documents.is_empty() {
            continue;
        }
        let extra_queries: Vec<String> = project
            .touched_path
            .as_ref()
            .map(|p| vec![p.clone()])
            .unwrap_or_default();
        let ranked = rank(&project.documents, &extra_queries, Some(&project.identity_id));
        let qualified = ranked
            .into_iter()
            .map(|mut candidate| {
                candidate.path = format!("{PROJECT_MEMORY_PATH_PREFIX}{}/{}", project.identity_id, candidate.path);
                candidate
            })
            .collect();
        project_ranked_lists.push(qualified);
    }
    let project_ranked = interleave(&project_ranked_lists);
    let project_seats = max_items.div_ceil(2).min(project_ranked.len());
    let own_take_count = max_items.saturating_sub(project_seats);
    let own_taken: Vec<RecallCandidate> = own_ranked.into_iter().take(own_take_count).collect();
    let project_take_count = max_items.saturating_sub(own_taken.len());
    let project_taken: Vec<RecallCandidate> = project_ranked.into_iter().take(project_take_count).collect();

    let mut result = own_taken;
    result.extend(project_taken);
    result
}

fn interleave(lists: &[Vec<RecallCandidate>]) -> Vec<RecallCandidate> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let max_len = lists.iter().map(|l| l.len()).max().unwrap_or(0);
    for index in 0..max_len {
        for list in lists {
            if let Some(candidate) = list.get(index)
                && !seen.contains(&candidate.path) {
                    seen.insert(candidate.path.clone());
                    out.push(candidate.clone());
                }
        }
    }
    out
}

/// Qualified presentation for transcript filtering and sidecar reads.
pub fn project_documents_of(projects: &[ProjectRecallCorpus]) -> Vec<RecallDocument> {
    projects
        .iter()
        .flat_map(|project| qualify_project_documents(&project.identity_id, &project.documents))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use memory_core::recall::{select_recall_candidates, SelectRecallOptions};

    const QUERY: &str = "kubernetes rollout canary";

    fn note(path: &str, description: &str, body: &str) -> RecallDocument {
        RecallDocument {
            path: path.to_string(),
            description: description.to_string(),
            body: body.to_string(),
        }
    }

    fn filler(prefix: &str, count: usize, text: &str) -> Vec<RecallDocument> {
        (0..count)
            .map(|index| note(&format!("{prefix}/note-{index}.md"), &format!("{text} {index}"), &format!("{text} {index}")))
            .collect()
    }

    fn project(id: &str, documents: Vec<RecallDocument>, touched_path: Option<&str>) -> ProjectRecallCorpus {
        ProjectRecallCorpus {
            identity_id: id.to_string(),
            documents,
            touched_path: touched_path.map(str::to_string),
        }
    }

    fn rank_fn(queries: Vec<String>, max_items: usize) -> impl Fn(&[RecallDocument], &[String], Option<&str>) -> Vec<RecallCandidate> {
        move |documents: &[RecallDocument], extra_queries: &[String], _identity_id: Option<&str>| {
            let mut all_queries = queries.clone();
            all_queries.extend(extra_queries.iter().cloned());
            select_recall_candidates(
                documents,
                &all_queries,
                &SelectRecallOptions {
                    max_items,
                    surfaced: BTreeSet::new(),
                    exclude_paths: None,
                    strategy: None,
                    expansions: None,
                },
            )
        }
    }

    #[test]
    fn given_150_own_notes_and_a_60_note_project_when_ranked_then_own_candidates_are_own_only_ranking() {
        let own = filler("own", 150, "kubernetes rollout canary steps");
        let projects = vec![project("child-1234abcd", filler("child", 60, "kubernetes rollout canary checklist"), None)];
        let max_items = 4;

        let alone = select_with_project_seats(&own, &[], max_items, rank_fn(vec![QUERY.to_string()], max_items));
        let with_project = select_with_project_seats(&own, &projects, max_items, rank_fn(vec![QUERY.to_string()], max_items));

        let own_part: Vec<_> = with_project.iter().filter(|c| !c.path.starts_with("@project/")).cloned().collect();
        assert_eq!(own_part, alone[..own_part.len()]);
        assert_eq!(own_part.len(), max_items - max_items.div_ceil(2));
        assert!(with_project.iter().any(|c| c.path.starts_with("@project/")));
    }

    #[test]
    fn given_no_project_note_matches_when_ranked_then_own_notes_take_every_seat() {
        let own = filler("own", 5, "kubernetes rollout canary");
        let unrelated = vec![project("child-1234abcd", vec![note("cooking.md", "pasta recipes", "pasta recipes")], None)];

        let own_only = select_with_project_seats(&own, &unrelated, 4, rank_fn(vec![QUERY.to_string()], 4));
        assert_eq!(own_only.len(), 4);
        assert!(own_only.iter().all(|c| !c.path.starts_with("@project/")));
    }

    #[test]
    fn parse_and_qualify_roundtrip() {
        let docs = vec![note("sub/doc.md", "desc", "body")];
        let qualified = qualify_project_documents("agent-1", &docs);
        assert_eq!(qualified[0].path, "@project/agent-1/sub/doc.md");
        let (id, path) = parse_project_memory_path(&qualified[0].path).unwrap();
        assert_eq!(id, "agent-1");
        assert_eq!(path, "sub/doc.md");
    }
}
