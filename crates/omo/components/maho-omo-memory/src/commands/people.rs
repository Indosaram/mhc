//! `/people [<name>] [--all] [--ask "<question>"]`: roster, relationship graph, and
//! dialectic-lite queries over the people records.
//! Port of `components/memory/commands/people.ts` at pin 77f3067f1.

use std::{path::Path, sync::Arc};

use maho_ext_api::ExtensionApi;
use memory_core::people::format::{ObservationGroup, PeopleLimits};

use crate::palace::{PalacePeople, PalacePeopleNode, collect_people};
use crate::model_registry_resolver::NativeMemoryModelRegistry;

use super::args::{ParseCommandArgsOptions, parse_command_args_full};
use super::people_ask::{
    ABSTENTION_LINE, PeopleAskEvidence, PeopleAskOptions, PeopleAskRequest,
    create_people_ask_runner, has_no_evidence,
};
use super::people_query::{PersonQueryResolution, resolve_person_query};
use super::people_render::{parse_people_card_lines, parse_people_observations, render_person_card, render_roster};
use super::people_search::collect_person_search_hits;
use super::repo::open_repo;
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, MemoryCommandIdentity, NotifyLevel,
};

/// Fixed render budget per observation level; deliberately NOT a config knob.
pub const OBSERVATIONS_PER_LEVEL: usize = 20;

pub use super::people_query::{PersonQueryResolution as PersonQuery, resolve_person_query as resolve_person};

/// Derives the roster and adjacency graph. A repository-open, `head`, or `ls_tree`
/// failure propagates as an error; only a successful `None` (people disabled) defaults
/// to an empty graph, matching the pinned command's error behavior.
pub fn derive_people_graph(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
    limits: PeopleLimits,
) -> Result<PalacePeople, String> {
    let repo = open_repo(deps, identity)?;
    let head = repo.head().map_err(|error| error.to_string())?;
    let options = crate::palace::PalacePeopleOptions { enabled: true, limits };
    resolve_people_graph(
        collect_people(&repo, head.as_deref(), &options).map_err(|error| error.to_string()),
    )
}

/// Maps the collector carrier: a successful `None` (disabled) defaults to an empty graph,
/// while an error is propagated so a failed collection never renders as an empty roster.
pub fn resolve_people_graph(
    collected: Result<Option<PalacePeople>, String>,
) -> Result<PalacePeople, String> {
    match collected {
        Ok(Some(people)) => Ok(people),
        Ok(None) => Ok(empty_people_graph()),
        Err(error) => Err(error),
    }
}

fn empty_people_graph() -> PalacePeople {
    PalacePeople { nodes: Vec::new(), edges: Vec::new(), diagnostics: Vec::new() }
}

pub fn select_observations(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
    slug: &str,
    limits: PeopleLimits,
    all: bool,
) -> Vec<ObservationGroup> {
    let path = format!("people/{slug}/observations.md");
    let Some(raw) = read_person_file(deps, identity, &path) else {
        return Vec::new();
    };
    parse_people_observations(Some(&raw), limits)
        .into_iter()
        .map(|group| {
            let mut sorted = group.entries;
            sorted.sort_by(|left, right| right.date.cmp(&left.date));
            if !all {
                sorted.truncate(OBSERVATIONS_PER_LEVEL);
            }
            ObservationGroup { section: group.section, entries: sorted }
        })
        .collect()
}

pub async fn handle_people(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let settings = match (deps.settings)() {
        Ok(settings) => settings,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let people = &settings["people"];
    if people["enabled"].as_bool() == Some(false) {
        return respond(ctx, "people records are disabled (memory.people.enabled)", NotifyLevel::Error);
    }
    let limits = PeopleLimits {
        max_entries: people["max_entries"].as_u64().unwrap_or(40) as usize,
        max_entry_chars: people["max_entry_chars"].as_u64().unwrap_or(200) as usize,
    };

    let (question, rest) = extract_ask(args);
    let parsed = parse_command_args_full(&rest, ParseCommandArgsOptions { booleans: &["all"] });
    let graph = match derive_people_graph(deps, &identity, limits) {
        Ok(graph) => graph,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let name = parsed.positionals.join(" ").trim().to_owned();
    if name.is_empty() {
        return respond(ctx, render_roster(&graph), NotifyLevel::Info);
    }

    let resolved = resolve_person_query(&graph.nodes, &name);
    let slug = match resolved {
        PersonQueryResolution::Hit { slug } => slug,
        PersonQueryResolution::Miss { close } => {
            let close = if close.is_empty() {
                "no people records exist yet".to_owned()
            } else {
                format!("closest slugs: {}", close.join(", "))
            };
            return respond(
                ctx,
                format!("no person matches \"{name}\"; {close}"),
                NotifyLevel::Error,
            );
        }
    };
    let Some(node) = graph.nodes.iter().find(|node| node.slug == slug) else {
        return respond(ctx, format!("no person matches \"{name}\""), NotifyLevel::Error);
    };

    let observations = select_observations(
        deps,
        &identity,
        &node.slug,
        limits,
        question.is_none() && parsed.flag_is_true("all"),
    );
    let card_body = read_person_file(deps, &identity, &node.path);

    if let Some(question) = question {
        return ask_about_person(deps, ctx, node, &question, limits, card_body.as_deref(), &observations, &graph).await;
    }

    respond(
        ctx,
        render_person_card(
            node,
            &parse_people_card_lines(card_body.as_deref(), limits),
            &observations,
            &graph.edges,
        ),
        NotifyLevel::Info,
    )
}

async fn ask_about_person(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    node: &PalacePeopleNode,
    question: &str,
    limits: PeopleLimits,
    card_body: Option<&str>,
    observations: &[ObservationGroup],
    _graph: &PalacePeople,
) -> CommandResponse {
    let evidence = PeopleAskEvidence {
        card: parse_people_card_lines(card_body, limits),
        observations: observations
            .iter()
            .flat_map(|group| {
                group
                    .entries
                    .iter()
                    .map(|entry| format!("[{}] {} {}", group.section, entry.date, entry.content))
            })
            .collect(),
        search_hits: collect_person_search_hits(deps, ctx, node),
    };
    if has_no_evidence(&evidence) {
        return respond(ctx, ABSTENTION_LINE, NotifyLevel::Info);
    }

    let runner = match &deps.people_ask {
        Some(runner) => runner.clone(),
        None => {
            let registry = ctx
                .model_registry
                .clone()
                .map(|registry| Arc::new(NativeMemoryModelRegistry(registry)) as Arc<dyn senpi_task::host::SenpiModelRegistry>);
            create_people_ask_runner(PeopleAskOptions {
                config: deps.full_config_value(),
                registry,
                env: deps.environment(),
                launcher: None,
                deadline_ms: None,
            })
        }
    };
    let request = PeopleAskRequest {
        slug: node.slug.clone(),
        display_name: node.display_name.clone(),
        question: question.to_owned(),
        evidence,
    };
    match runner(request).await {
        Ok(answer) => respond(ctx, answer, NotifyLevel::Info),
        Err(error) => respond(ctx, error, NotifyLevel::Error),
    }
}

fn read_person_file(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
    path: &str,
) -> Option<String> {
    let working = std::fs::read_to_string(identity.identity_paths.repo.join(path)).ok();
    if working.is_some() {
        return working;
    }
    let repo = open_repo(deps, identity).ok()?;
    let head = repo.head().ok().flatten()?;
    repo.show(&head, path).ok()
}

/// `--ask` carries a quoted sentence, so its value is lifted out of the raw argument
/// string before the rest is parsed by the whitespace parser.
fn extract_ask(args: &str) -> (Option<String>, String) {
    let Some(index) = args.find("--ask") else {
        return (None, args.to_owned());
    };
    let after = &args[index + "--ask".len()..];
    let (separator_len, value_and_rest) = if let Some(rest) = after.strip_prefix('=') {
        (1, rest)
    } else if after.starts_with(char::is_whitespace) {
        let trimmed = after.trim_start();
        (after.len() - trimmed.len(), trimmed)
    } else {
        return (None, args.to_owned());
    };
    let _ = separator_len;
    if value_and_rest.is_empty() {
        return (None, args.to_owned());
    }
    let (raw_value, consumed) = if let Some(quote) = value_and_rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
        match value_and_rest[quote.len_utf8()..].find(quote) {
            Some(end) => {
                let value = &value_and_rest[..quote.len_utf8() + end + quote.len_utf8()];
                (value.to_owned(), value.len())
            }
            None => {
                let value = value_and_rest.split_whitespace().next().unwrap_or("").to_owned();
                let len = value.len();
                (value, len)
            }
        }
    } else {
        let value = value_and_rest.split_whitespace().next().unwrap_or("").to_owned();
        let len = value.len();
        (value, len)
    };
    let unquoted = {
        let bytes = raw_value.as_bytes();
        if bytes.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') && bytes[bytes.len() - 1] == bytes[0] {
            raw_value[1..raw_value.len() - 1].to_owned()
        } else {
            raw_value.clone()
        }
    };
    let question = unquoted.trim().to_owned();
    let match_end = index + "--ask".len() + separator_len + consumed;
    let rest = format!("{} {}", &args[..index], &args[match_end..]);
    (
        if question.is_empty() { None } else { Some(question) },
        rest,
    )
}

pub fn register_people_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "people",
        Some(
            "Show the people roster and relationship graph, one person's record, or ask about them."
                .to_owned(),
        ),
        Some("[<name>] [--all] [--ask \"<question>\"]".to_owned()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_people(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::people_test_support::{LIMITS, people_fixture};
    use crate::commands::test_support::*;

    #[tokio::test]
    async fn given_three_cards_with_four_relationship_lines_when_the_graph_is_derived_then_nodes_and_edges_match() {
        let (_root, identity) = people_fixture(0);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());

        let graph = derive_people_graph(&fake.deps, &identity, LIMITS).expect("people graph");

        let mut slugs: Vec<String> = graph.nodes.iter().map(|node| node.slug.clone()).collect();
        slugs.sort();
        assert_eq!(slugs, ["human", "jane-doe", "nia-blank", "sam-rivers"]);
        let edges: Vec<(String, String, String, Option<String>)> = graph
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.source.clone(),
                    edge.predicate.clone(),
                    edge.target.clone(),
                    edge.target_slug.clone(),
                )
            })
            .collect();
        assert_eq!(
            edges,
            vec![
                ("human".to_owned(), "works-with".to_owned(), "jane-doe".to_owned(), Some("jane-doe".to_owned())),
                ("human".to_owned(), "mentors".to_owned(), "sam-rivers".to_owned(), Some("sam-rivers".to_owned())),
                ("jane-doe".to_owned(), "reports-to".to_owned(), "human".to_owned(), Some("human".to_owned())),
                ("sam-rivers".to_owned(), "collaborates".to_owned(), "unknown-person".to_owned(), None),
            ]
        );
    }

    #[tokio::test]
    async fn given_a_repository_without_any_card_when_the_graph_is_derived_then_it_holds_no_node_and_no_edge() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, vec![seed("system/persona.md", "---\ndescription: P\n---\nbody\n")]);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());

        let graph = derive_people_graph(&fake.deps, &identity, LIMITS).expect("people graph");

        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn given_a_collector_error_when_the_graph_is_resolved_then_the_error_propagates_instead_of_an_empty_roster() {
        let error = resolve_people_graph(Err("git ls-tree failed".to_owned()));
        assert_eq!(error, Err("git ls-tree failed".to_owned()));
    }

    #[test]
    fn given_a_disabled_collector_when_the_graph_is_resolved_then_it_defaults_to_an_empty_graph() {
        let graph = resolve_people_graph(Ok(None)).expect("disabled collector defaults");
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
        assert!(graph.diagnostics.is_empty());
    }

    #[test]
    fn given_a_collected_graph_when_resolved_then_the_carrier_passes_through_unchanged() {
        let collected = PalacePeople {
            nodes: vec![PalacePeopleNode {
                slug: "jane-doe".to_owned(),
                path: "people/jane-doe/card.md".to_owned(),
                display_name: "Jane Doe".to_owned(),
                kind: "person".to_owned(),
                aliases: vec!["JD".to_owned()],
                state: "committed".to_owned(),
            }],
            edges: Vec::new(),
            diagnostics: Vec::new(),
        };
        let graph = resolve_people_graph(Ok(Some(collected))).expect("passes through");
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].slug, "jane-doe");
        assert_eq!(graph.nodes[0].aliases, vec!["JD".to_owned()]);
    }

    #[tokio::test]
    async fn given_an_alias_when_the_query_resolves_then_it_hits_the_owning_slug() {
        let (_root, identity) = people_fixture(0);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let graph = derive_people_graph(&fake.deps, &identity, LIMITS).expect("people graph");

        assert_eq!(
            resolve_person_query(&graph.nodes, "jd"),
            PersonQueryResolution::Hit { slug: "jane-doe".to_owned() }
        );
    }

    #[tokio::test]
    async fn given_an_unknown_name_when_the_query_resolves_then_it_misses_with_close_slugs() {
        let (_root, identity) = people_fixture(0);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let graph = derive_people_graph(&fake.deps, &identity, LIMITS).expect("people graph");

        assert_eq!(
            resolve_person_query(&graph.nodes, "jane doh"),
            PersonQueryResolution::Miss { close: vec!["jane-doe".to_owned()] }
        );
    }

    #[tokio::test]
    async fn given_a_fixture_repository_when_people_runs_then_the_roster_is_surfaced_through_one_notification() {
        let (_root, identity) = people_fixture(0);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let graph = derive_people_graph(&fake.deps, &identity, LIMITS).expect("people graph");
        let response = handle_people(&fake.deps, &context.ctx, "").await;

        let slugs: std::collections::BTreeSet<String> =
            graph.nodes.iter().map(|node| node.slug.clone()).collect();
        assert_eq!(
            slugs,
            ["human", "jane-doe", "nia-blank", "sam-rivers"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            context.ui.notifications(),
            vec![(response.text.clone(), NotifyLevel::Info)]
        );
    }

    #[tokio::test]
    async fn given_an_unknown_name_when_people_runs_then_the_miss_preserves_close_slugs_and_errors() {
        let (_root, identity) = people_fixture(0);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_people(&fake.deps, &context.ctx, "jane doh").await;

        assert_eq!(
            context.ui.notifications(),
            vec![(response.text.clone(), NotifyLevel::Error)]
        );
    }

    #[tokio::test]
    async fn given_people_disabled_when_people_runs_then_it_refuses_without_reading_any_record() {
        let (_root, identity) = people_fixture(0);
        let mut settings = memory_settings();
        settings["people"] = serde_json::json!({ "enabled": false, "max_entries": 40, "max_entry_chars": 200 });
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { settings: Some(settings), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_people(&fake.deps, &context.ctx, "").await;

        assert_eq!(
            context.ui.notifications(),
            vec![(response.text.clone(), NotifyLevel::Error)]
        );
    }

    #[tokio::test]
    async fn given_no_evidence_for_the_person_when_ask_runs_then_it_abstains_without_launching_a_child() {
        let (_root, identity) = people_fixture(0);
        let fake = fake_deps(Some(identity), FakeDepsOverrides { people_ask: Some(true), ..Default::default() });
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_people(&fake.deps, &context.ctx, "nia-blank --ask \"does nia ship on fridays?\"").await;

        assert!(has_no_evidence(&PeopleAskEvidence::default()));
        assert!(fake.people_asks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
        assert_eq!(context.ui.notifications(), vec![(response.text.clone(), NotifyLevel::Info)]);
    }

    #[tokio::test]
    async fn given_card_and_observation_evidence_when_ask_runs_then_the_quick_child_receives_that_evidence() {
        let (_root, identity) = people_fixture(3);
        let fake = fake_deps(Some(identity), FakeDepsOverrides { people_ask: Some(true), ..Default::default() });
        let context = fake_command_context(FakeContextOptions::default());

        handle_people(&fake.deps, &context.ctx, "jane-doe --ask \"how does jane review?\"").await;

        let asked = fake.people_asks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].question, "how does jane review?");
        assert_eq!(asked[0].slug, "jane-doe");
        assert_eq!(
            asked[0].evidence.card,
            vec![
                "IDENTITY: staff engineer".to_owned(),
                "ATTRIBUTE: prefers small diffs".to_owned(),
                "RELATIONSHIP: reports-to: human".to_owned(),
            ]
        );
        assert_eq!(asked[0].evidence.observations.len(), 4);
        assert!(!has_no_evidence(&asked[0].evidence));
        assert_eq!(context.ui.notifications(), vec![(context.ui.notifications()[0].0.clone(), NotifyLevel::Info)]);
    }

    #[tokio::test]
    async fn given_more_observations_than_the_per_level_cap_when_selected_then_each_level_keeps_the_newest_twenty() {
        let (_root, identity) = people_fixture(25);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());

        let groups = select_observations(&fake.deps, &identity, "jane-doe", LIMITS, false);

        let explicit = groups.iter().find(|group| group.section == "Explicit").expect("explicit");
        assert_eq!(OBSERVATIONS_PER_LEVEL, 20);
        assert_eq!(explicit.entries.len(), 20);
        assert_eq!(explicit.entries[0].date, "2026-03-25");
        assert_eq!(explicit.entries.last().map(|entry| entry.date.clone()), Some("2026-03-06".to_owned()));
        assert_eq!(
            groups.iter().find(|group| group.section == "Inductive").map(|group| group.entries.len()),
            Some(1)
        );
    }

    #[tokio::test]
    async fn given_the_all_flag_when_observations_are_selected_then_every_entry_survives() {
        let (_root, identity) = people_fixture(25);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());

        let groups = select_observations(&fake.deps, &identity, "jane-doe", LIMITS, true);

        assert_eq!(
            groups.iter().find(|group| group.section == "Explicit").map(|group| group.entries.len()),
            Some(25)
        );
    }

    #[tokio::test]
    async fn given_a_person_with_observations_when_people_name_runs_then_card_fields_and_capped_selection_are_surfaced() {
        let (_root, identity) = people_fixture(25);
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let card = memory_core::people::format::parse_people_card(
            &memory_core::memfs::parse_memory_file(crate::commands::people_test_support::JANE_CARD)
                .expect("card file")
                .body,
            LIMITS,
        )
        .card;
        let observations = select_observations(&fake.deps, &identity, "jane-doe", LIMITS, false);
        let response = handle_people(&fake.deps, &context.ctx, "Jane").await;

        let entries: Vec<(String, String)> = card
            .entries
            .iter()
            .map(|entry| (entry.prefix.clone(), entry.content.clone()))
            .collect();
        assert_eq!(
            entries,
            vec![
                ("IDENTITY".to_owned(), "staff engineer".to_owned()),
                ("ATTRIBUTE".to_owned(), "prefers small diffs".to_owned()),
                ("RELATIONSHIP".to_owned(), "reports-to: human".to_owned()),
            ]
        );
        let dates: Vec<String> = observations
            .iter()
            .find(|group| group.section == "Explicit")
            .expect("explicit")
            .entries
            .iter()
            .map(|entry| entry.date.clone())
            .collect();
        let expected: Vec<String> = (0..20).map(|index| format!("2026-03-{:02}", 25 - index)).collect();
        assert_eq!(dates, expected);
        assert_eq!(context.ui.notifications(), vec![(response.text.clone(), NotifyLevel::Info)]);
    }
}
