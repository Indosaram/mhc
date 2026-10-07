use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use maho_ext_api::ExtensionFailure;
use maho_omo_bundled_skills::{HostCommandInfo, HostCommands};
use maho_omo_skill_commands::autocomplete::wrap_with_bare_skill_commands;
use maho_tui::autocomplete::{ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions};

fn names() -> BTreeSet<String> {
    ["ulw-execute", "ulw-plan", "ulw-research", "init-deep"].iter().map(|name| (*name).to_owned()).collect()
}

fn commands() -> Vec<HostCommandInfo> {
    [
        ("skill:ulw-execute", Some("Executes a work plan.")),
        ("skill:ulw-plan", Some("Plans first.")),
        ("skill:init-deep", None),
        ("init", None),
    ]
    .iter()
    .map(|(name, description)| HostCommandInfo {
        name: (*name).to_owned(),
        description: description.map(str::to_owned),
        source: if name.starts_with("skill:") { "skill" } else { "extension" }.to_owned(),
        source_info_path: None,
    })
    .collect()
}

fn host_commands() -> HostCommands {
    Arc::new(|| -> Result<Option<Vec<HostCommandInfo>>, ExtensionFailure> { Ok(Some(commands())) })
}

struct FakeProvider {
    result: Option<AutocompleteSuggestions>,
    applied: Mutex<usize>,
}

impl FakeProvider {
    fn new(result: Option<AutocompleteSuggestions>) -> Self {
        Self { result, applied: Mutex::new(0) }
    }
}

impl AutocompleteProvider for FakeProvider {
    fn trigger_characters(&self) -> Vec<String> {
        vec!["$".to_owned()]
    }

    fn get_suggestions(&mut self, _: &[String], _: usize, _: usize, _: bool) -> Option<AutocompleteSuggestions> {
        self.result.clone()
    }

    fn apply_completion(&self, _: &[String], _: usize, _: usize, _: &AutocompleteItem, _: &str) -> ApplyCompletionResult {
        *self.applied.lock().expect("applied") += 1;
        ApplyCompletionResult { lines: vec!["applied-by-base".to_owned()], cursor_line: 0, cursor_col: 0 }
    }
}

fn item(value: &str) -> AutocompleteItem {
    AutocompleteItem { value: value.to_owned(), label: value.to_owned(), description: None }
}

fn values(items: &[AutocompleteItem]) -> Vec<String> {
    items.iter().map(|item| item.value.clone()).collect()
}

#[test]
fn given_senpi_lists_skill_rows_when_the_user_types_a_prefix_then_each_loaded_alias_sits_above_its_own_skill_row() {
    let base = FakeProvider::new(Some(AutocompleteSuggestions {
        prefix: "/ulw".to_owned(),
        items: vec![item("skill:ulw-execute"), item("skill:ulw-plan")],
    }));
    let mut wrapped = wrap_with_bare_skill_commands(Box::new(base), names(), host_commands());

    let result = wrapped.get_suggestions(&["/ulw".to_owned()], 0, 4, false).expect("suggestions");

    assert_eq!(result.prefix, "/ulw");
    assert_eq!(values(&result.items), ["ulw-execute", "skill:ulw-execute", "ulw-plan", "skill:ulw-plan"]);
    assert_eq!(result.items[0].description.as_deref(), Some("Executes a work plan."));
}

#[test]
fn given_the_skill_row_carries_an_argument_hint_when_the_user_types_a_prefix_then_the_alias_shows_the_same_description() {
    let base = FakeProvider::new(Some(AutocompleteSuggestions {
        prefix: "/ulw".to_owned(),
        items: vec![
            AutocompleteItem {
                value: "skill:ulw-execute".to_owned(),
                label: "skill:ulw-execute".to_owned(),
                description: Some("[plan-name] — Executes a work plan.".to_owned()),
            },
            AutocompleteItem { value: "skill:ulw-plan".to_owned(), label: "skill:ulw-plan".to_owned(), description: Some("Plans first.".to_owned()) },
        ],
    }));
    let mut wrapped = wrap_with_bare_skill_commands(Box::new(base), names(), host_commands());

    let result = wrapped.get_suggestions(&["/ulw".to_owned()], 0, 4, false).expect("suggestions");

    assert_eq!(
        result.items[0],
        AutocompleteItem {
            value: "ulw-execute".to_owned(),
            label: "ulw-execute".to_owned(),
            description: Some("[plan-name] — Executes a work plan.".to_owned()),
        }
    );
    assert_eq!(
        result.items[2],
        AutocompleteItem { value: "ulw-plan".to_owned(), label: "ulw-plan".to_owned(), description: Some("Plans first.".to_owned()) }
    );
}

#[test]
fn given_a_bundled_skill_is_disabled_when_the_user_types_its_prefix_then_its_alias_is_not_offered() {
    let mut wrapped = wrap_with_bare_skill_commands(Box::new(FakeProvider::new(None)), names(), host_commands());

    assert_eq!(wrapped.get_suggestions(&["/ulw-r".to_owned()], 0, 6, false), None);
}

#[test]
fn given_senpi_has_no_suggestion_page_when_the_user_types_a_prefix_then_the_alias_alone_is_offered_with_the_typed_prefix() {
    let mut wrapped = wrap_with_bare_skill_commands(Box::new(FakeProvider::new(None)), names(), host_commands());

    assert_eq!(
        wrapped.get_suggestions(&["/init-d".to_owned()], 0, 7, false),
        Some(AutocompleteSuggestions { prefix: "/init-d".to_owned(), items: vec![item("init-deep")] })
    );
}

#[test]
fn given_inputs_outside_a_leading_command_token_when_suggestions_are_requested_then_senpis_page_is_returned_unchanged() {
    let page = AutocompleteSuggestions { prefix: "/".to_owned(), items: vec![item("settings")] };
    let mut wrapped = wrap_with_bare_skill_commands(Box::new(FakeProvider::new(Some(page.clone()))), names(), host_commands());

    assert_eq!(wrapped.get_suggestions(&["/".to_owned()], 0, 1, false), Some(page.clone()));
    assert_eq!(wrapped.get_suggestions(&["/ulw-execute plan".to_owned()], 0, 17, false), Some(page.clone()));
    assert_eq!(wrapped.get_suggestions(&["first".to_owned(), "/ulw".to_owned()], 1, 4, false), Some(page.clone()));
    assert_eq!(wrapped.get_suggestions(&["/ulw".to_owned()], 0, 4, true), Some(page));
}

#[test]
fn given_the_wrapped_provider_when_other_members_are_used_then_they_are_the_base_providers_own() {
    let base = FakeProvider::new(None);
    let mut wrapped = wrap_with_bare_skill_commands(Box::new(base), names(), host_commands());

    let applied = wrapped.apply_completion(&["/ulw".to_owned()], 0, 4, &item("ulw-plan"), "/ulw");
    assert_eq!(applied.lines, ["applied-by-base"]);
    assert_eq!(wrapped.trigger_characters(), ["$"]);
}
