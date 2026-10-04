use std::collections::BTreeSet;
use std::path::Path;

use memory_core::git::GitMemoryRepo;
use memory_core::memfs::parse_memory_file;
use memory_core::people::{PeopleLimits, parse_people_card};
use serde::Serialize;

use super::PalaceError;
use super::entry_collector::PalaceEntryState;

pub const PRIMARY_HUMAN_SLUG: &str = "human";
pub const PRIMARY_HUMAN_CARD_PATH: &str = "system/human.md";

const PEOPLE_PREFIX: &str = "people/";
const CARD_SUFFIX: &str = "/card.md";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalacePeopleNode {
    pub slug: String,
    pub path: String,
    pub display_name: String,
    pub kind: String,
    pub aliases: Vec<String>,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalacePeopleEdge {
    pub source: String,
    pub predicate: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_slug: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalacePeopleDiagnostic {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PalacePeople {
    pub nodes: Vec<PalacePeopleNode>,
    pub edges: Vec<PalacePeopleEdge>,
    pub diagnostics: Vec<PalacePeopleDiagnostic>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PalacePeopleOptions {
    pub enabled: bool,
    pub limits: PeopleLimits,
}

impl Default for PalacePeopleOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            limits: PeopleLimits {
                max_entries: 40,
                max_entry_chars: 200,
            },
        }
    }
}

struct Relationship {
    source: String,
    predicate: String,
    target: String,
}

pub fn collect_people(
    repo: &GitMemoryRepo,
    head: Option<&str>,
    options: &PalacePeopleOptions,
) -> Result<Option<PalacePeople>, PalaceError> {
    if !options.enabled {
        return Ok(None);
    }

    let committed = match head {
        Some(head) => repo
            .ls_tree(Some(head), None)?
            .into_iter()
            .filter(|path| is_card_path(path.as_str()))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let dirty = dirty_paths(repo);
    let working = working_card_paths(&repo.dir);
    let committed_set: BTreeSet<String> = committed.iter().cloned().collect();
    let mut paths: Vec<String> = committed
        .into_iter()
        .chain(working)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    paths.sort_by(|left, right| by_primary_human_first(left, right));

    let mut nodes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut relationships = Vec::new();

    for path in paths {
        let state = if dirty.contains(&path) || head.is_none() || !committed_set.contains(&path) {
            PalaceEntryState::Uncommitted
        } else {
            PalaceEntryState::Committed
        };
        let Some(raw) = read_card(repo, head, &path, state) else {
            continue;
        };
        let Some(file) = parse_file(&raw) else {
            diagnostics.push(PalacePeopleDiagnostic {
                path,
                message: "card frontmatter is unreadable".to_string(),
            });
            continue;
        };
        let slug = slug_of(&path);
        let card_path = path.clone();
        let parsed = parse_people_card(&file.body, options.limits);
        for message in parsed.diagnostics {
            diagnostics.push(PalacePeopleDiagnostic {
                path: card_path.clone(),
                message,
            });
        }
        nodes.push(PalacePeopleNode {
            slug: slug.clone(),
            path,
            display_name: display_name_of(&file.frontmatter.description, &slug),
            kind: file.frontmatter.kind.unwrap_or_else(|| "person".to_string()),
            aliases: file.frontmatter.aliases.unwrap_or_default(),
            state: state.label().to_string(),
        });
        for entry in parsed.card.entries {
            if entry.prefix != "RELATIONSHIP" {
                continue;
            }
            match parse_relationship(&slug, &entry.content) {
                Some(edge) => relationships.push(edge),
                None => {
                    let content = &entry.content;
                    diagnostics.push(PalacePeopleDiagnostic {
                        path: card_path.clone(),
                        message: format!(
                            "RELATIONSHIP entry is not '<predicate>: <target>': {content}"
                        ),
                    });
                }
            }
        }
    }

    let known: BTreeSet<String> = nodes.iter().map(|node| node.slug.clone()).collect();
    let edges = relationships
        .into_iter()
        .map(|edge| PalacePeopleEdge {
            target_slug: known.contains(&edge.target).then(|| edge.target.clone()),
            source: edge.source,
            predicate: edge.predicate,
            target: edge.target,
        })
        .collect();
    Ok(Some(PalacePeople {
        nodes,
        edges,
        diagnostics,
    }))
}

fn parse_relationship(source: &str, content: &str) -> Option<Relationship> {
    let separator = content.find(':')?;
    if separator == 0 {
        return None;
    }
    let predicate = content[..separator].trim();
    let target = content[separator + 1..].trim();
    if predicate.is_empty() || target.is_empty() {
        return None;
    }
    Some(Relationship {
        source: source.to_string(),
        predicate: predicate.to_string(),
        target: target.to_string(),
    })
}

fn read_card(
    repo: &GitMemoryRepo,
    head: Option<&str>,
    path: &str,
    state: PalaceEntryState,
) -> Option<String> {
    if state == PalaceEntryState::Uncommitted {
        return std::fs::read_to_string(repo.dir.join(path)).ok();
    }
    let head = head?;
    repo.show(head, path).ok()
}

fn parse_file(raw: &str) -> Option<memory_core::memfs::ParsedMemoryFile> {
    parse_memory_file(raw).ok()
}

fn dirty_paths(repo: &GitMemoryRepo) -> BTreeSet<String> {
    let porcelain = repo.status(&[] as &[&str]).unwrap_or_default();
    let mut paths = BTreeSet::new();
    for line in porcelain.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let path = line.chars().skip(3).collect::<String>().trim().to_string();
        let path = path
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .map(str::to_string)
            .unwrap_or(path);
        if !path.is_empty() {
            paths.insert(path);
        }
    }
    paths
}

fn working_card_paths(repo_dir: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    if is_file(&repo_dir.join(PRIMARY_HUMAN_CARD_PATH)) {
        paths.push(PRIMARY_HUMAN_CARD_PATH.to_string());
    }
    if let Ok(entries) = std::fs::read_dir(repo_dir.join("people")) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let card = format!("{PEOPLE_PREFIX}{}{CARD_SUFFIX}", entry.file_name().to_string_lossy());
            if is_file(&repo_dir.join(&card)) {
                paths.push(card);
            }
        }
    }
    paths
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|info| info.is_file()).unwrap_or(false)
}

fn is_card_path(path: &str) -> bool {
    if path == PRIMARY_HUMAN_CARD_PATH {
        return true;
    }
    if !path.starts_with(PEOPLE_PREFIX) || !path.ends_with(CARD_SUFFIX) {
        return false;
    }
    let slug = slug_of(path);
    !slug.is_empty() && !slug.contains('/')
}

fn slug_of(path: &str) -> String {
    if path == PRIMARY_HUMAN_CARD_PATH {
        return PRIMARY_HUMAN_SLUG.to_string();
    }
    path[PEOPLE_PREFIX.len()..path.len() - CARD_SUFFIX.len()].to_string()
}

fn by_primary_human_first(left: &str, right: &str) -> std::cmp::Ordering {
    if left == PRIMARY_HUMAN_CARD_PATH {
        return std::cmp::Ordering::Less;
    }
    if right == PRIMARY_HUMAN_CARD_PATH {
        return std::cmp::Ordering::Greater;
    }
    left.cmp(right)
}

fn display_name_of(description: &str, slug: &str) -> String {
    if let Some(rest) = description.strip_prefix("Person")
        && rest.starts_with(char::is_whitespace)
    {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('-')
            && rest.starts_with(char::is_whitespace)
        {
            let name = rest.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    slug.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::entry_collector::UNCOMMITTED_LABEL;
    use crate::palace::generator::{GeneratePalaceOptions, generate_palace_html};
    use crate::palace::test_support::{create_palace_fixture, inline_json};
    use memory_core::git::{
        GitExecOptions, GitExecResult, GitExecRuntime, GitMemoryRepoOptions, create_git_exec,
    };
    use std::sync::Arc;

    const LIMITS: PeopleLimits = PeopleLimits {
        max_entries: 40,
        max_entry_chars: 200,
    };

    fn limits_options(enabled: bool) -> PalacePeopleOptions {
        PalacePeopleOptions { enabled, limits: LIMITS }
    }

    #[test]
    fn committed_cards_carry_slug_name_and_kind() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let people = collect_people(&fixture.repo, Some(fixture.head.as_str()), &limits_options(true))
            .unwrap()
            .unwrap();

        let mut slugs = people.nodes.iter().map(|node| node.slug.clone()).collect::<Vec<_>>();
        slugs.sort();
        assert_eq!(slugs, ["human", "jane-doe", "sam-rivers"]);
        let jane = people.nodes.iter().find(|node| node.slug == "jane-doe").unwrap();
        assert_eq!(jane.display_name, "Jane Doe");
        assert_eq!(jane.kind, "person");
        assert_eq!(jane.aliases, ["Jane", "JD"]);
    }

    #[test]
    fn relationship_lines_become_edges() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let people = collect_people(&fixture.repo, Some(fixture.head.as_str()), &limits_options(true))
            .unwrap()
            .unwrap();

        let edges = people
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.source.as_str(),
                    edge.predicate.as_str(),
                    edge.target.as_str(),
                    edge.target_slug.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            edges,
            [
                ("human", "works-with", "jane-doe", Some("jane-doe")),
                ("human", "mentors", "sam-rivers", Some("sam-rivers")),
                ("jane-doe", "reports-to", "human", Some("human")),
                ("sam-rivers", "collaborates", "unknown-person", None),
            ]
        );
    }

    #[test]
    fn non_relationship_lines_produce_no_edge() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let people = collect_people(&fixture.repo, Some(fixture.head.as_str()), &limits_options(true))
            .unwrap()
            .unwrap();

        assert!(!people.edges.iter().any(|edge| edge.predicate == "senior-engineer"));
        assert!(people.edges.iter().all(|edge| !edge.target.is_empty()));
    }

    #[test]
    fn uncommitted_card_is_labelled() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);
        fixture.write_working_file(
            "people/kim-lee/card.md",
            "---\ndescription: Person - Kim Lee\nkind: person\naliases: [\"Kim\"]\n---\n\nIDENTITY: draft person\n",
        );

        let people = collect_people(&fixture.repo, Some(fixture.head.as_str()), &limits_options(true))
            .unwrap()
            .unwrap();

        let kim = people.nodes.iter().find(|node| node.slug == "kim-lee").unwrap();
        assert_eq!(kim.state, UNCOMMITTED_LABEL);
        let jane = people.nodes.iter().find(|node| node.slug == "jane-doe").unwrap();
        assert_eq!(jane.state, "committed");
    }

    #[test]
    fn repo_without_people_directory_has_only_human() {
        let fixture = create_palace_fixture(false);

        let people = collect_people(&fixture.repo, Some(fixture.head.as_str()), &limits_options(true))
            .unwrap()
            .unwrap();

        assert_eq!(people.nodes.iter().map(|node| node.slug.as_str()).collect::<Vec<_>>(), ["human"]);
        assert!(people.edges.is_empty());
    }

    #[test]
    fn disabled_people_produces_no_section() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let people = collect_people(&fixture.repo, Some(fixture.head.as_str()), &limits_options(false))
            .unwrap();

        assert!(people.is_none());
    }

    #[test]
    fn git_failure_on_ls_tree_propagates_instead_of_masking_it() {
        let root = tempfile::tempdir().unwrap();
        let exec = create_git_exec(GitExecRuntime {
            platform: None,
            run_command: Some(Arc::new(|_: &str, _: &[String], _: &GitExecOptions| {
                Ok(GitExecResult {
                    code: 128,
                    stdout: String::new(),
                    stderr: "fatal: not a git repository (or any parent up to mount point /)".to_string(),
                })
            })),
        });
        let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
            dir: root.path().to_path_buf(),
            agent_id: "agent".to_string(),
            exec: Some(exec),
            install_hooks: None,
        })
        .unwrap();

        let result = collect_people(&repo, Some("HEAD"), &limits_options(true));

        assert!(matches!(result, Err(PalaceError::Git(_))));
    }

    #[test]
    fn oversize_entry_surfaces_diagnostic_with_card_path() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let people = collect_people(
            &fixture.repo,
            Some(fixture.head.as_str()),
            &PalacePeopleOptions {
                enabled: true,
                limits: PeopleLimits {
                    max_entries: 40,
                    max_entry_chars: 10,
                },
            },
        )
        .unwrap()
        .unwrap();

        assert!(people.diagnostics.iter().any(|entry| entry.path == "people/jane-doe/card.md"));
    }

    #[test]
    fn enabled_people_reaches_inline_payload() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let result = generate_palace_html(
            &fixture.context,
            GeneratePalaceOptions {
                people: Some(limits_options(true)),
                ..Default::default()
            },
        )
        .unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();
        let data = inline_json(&html);

        assert!(!data["people"].is_null());
        assert!(data["people"].to_string().contains("jane-doe"));
        assert!(html.contains("data-tab=\"people\""));
    }

    #[test]
    fn disabled_people_strips_panel_and_tab() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(false);

        let result = generate_palace_html(
            &fixture.context,
            GeneratePalaceOptions {
                people: Some(limits_options(false)),
                ..Default::default()
            },
        )
        .unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();
        let data = inline_json(&html);

        assert!(data.get("people").is_none());
        assert!(!html.contains("data-tab=\"people\""));
        assert!(!html.contains("id=\"panel-people\""));
    }

    #[test]
    fn script_breakout_payload_never_reaches_document() {
        let mut fixture = create_palace_fixture(false);
        fixture.seed_people(true);

        let result = generate_palace_html(
            &fixture.context,
            GeneratePalaceOptions {
                people: Some(limits_options(true)),
                ..Default::default()
            },
        )
        .unwrap();
        let html = std::fs::read_to_string(&result.path).unwrap();

        assert!(!html.contains("</script><img"));
        assert!(inline_json(&html).to_string().contains("onerror=alert(1)"));
    }
}
