use maho_ext_api::{ExtensionApi, ExtensionSessionProfile, LoadedExtension, SourceInfo, EventBus, ExtensionRuntime};
use maho_ext_rules::{commands::{register_slash_commands, find_rule_by_id}, config::config_from_environment, rules::{engine::Engine, types::*}};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn rules_completions_preserve_subcommand_order_and_empty_results() {
    // Given
    let mut api = ExtensionApi::new(LoadedExtension::new("rules", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    register_slash_commands(&mut api, Arc::new(Mutex::new(Engine::new(config_from_environment(|_| None), "/tmp".into()))));
    let complete = &api.registered.command_argument_completions["rules"];
    // When
    let all = complete("").await.unwrap().unwrap();
    let matching = complete("s").await.unwrap().unwrap();
    let missing = complete("unknown").await.unwrap();
    // Then
    assert_eq!(all.iter().map(|item| item.value.as_str()).collect::<Vec<_>>(), ["list", "show", "paths", "status"]);
    assert_eq!(matching.iter().map(|item| item.value.as_str()).collect::<Vec<_>>(), ["show", "status"]);
    assert!(missing.is_none());
}

#[test]
fn registers_both_commands() {
    let mut api = ExtensionApi::new(LoadedExtension::new("rules", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    register_slash_commands(&mut api, Arc::new(Mutex::new(Engine::new(config_from_environment(|_| None), "/tmp".into()))));
    let names: Vec<_> = api.registered.commands.iter().map(|command| command.name.as_str()).collect();
    assert_eq!(names, ["rules", "reload-rules"]);
}
#[test]
fn rules_extension_registers_all_lifecycle_hooks() {
    use maho_ext_api::{Extension, EventKind};
    let mut api = ExtensionApi::new(LoadedExtension::new("rules", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_rules::Rules.register(&mut api);
    for kind in [EventKind::SessionStart, EventKind::SessionCompact, EventKind::BeforeAgentStart, EventKind::ToolResult] { assert_eq!(api.registered.handlers[&kind].len(), 1); }
    assert_eq!(api.registered.flags.len(), 2);
    assert!(api.registered.entry_renderers.contains_key("rule-activation"));
}
#[test]
fn rule_lookup_requires_exact_or_unique_suffix() {
    let rules: Vec<_> = ["one/r.md", "two/r.md"].into_iter().map(|path| LoadedRule {
        candidate: RuleCandidate { path: path.into(), real_path: path.into(), source: ".omo/rules".into(), distance: 0, is_global: false, is_single_file: false, relative_path: path.into() },
        frontmatter: RuleFrontmatter::default(), body: String::new(), content_hash: String::new(), match_reason: MatchReason::NoMatch,
    }).collect();
    assert_eq!(find_rule_by_id(&rules, "one/r.md").unwrap().candidate.relative_path, "one/r.md");
    assert!(find_rule_by_id(&rules, "r.md").is_none());
    assert!(find_rule_by_id(&rules, "missing").is_none());
    assert!(find_rule_by_id(&rules, "").is_none());
}
