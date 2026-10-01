use maho_ext_api::{ExtensionApi, ExtensionSessionProfile, LoadedExtension, SourceInfo, EventBus, ExtensionRuntime};
use maho_ext_rules::{commands::{register_slash_commands, find_rule_by_id}, config::config_from_environment, rules::{engine::Engine, types::*}};
use std::sync::{Arc, Mutex};

#[test]
fn registers_both_commands() {
    let mut api = ExtensionApi::new(LoadedExtension::new("rules", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    register_slash_commands(&mut api, Arc::new(Mutex::new(Engine::new(config_from_environment(|_| None), "/tmp".into()))));
    let names: Vec<_> = api.registered.commands.iter().map(|command| command.name.as_str()).collect();
    assert_eq!(names, ["rules", "reload-rules"]);
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
