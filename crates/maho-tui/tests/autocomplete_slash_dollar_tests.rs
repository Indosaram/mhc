//! Port of senpi `packages/tui/test/autocomplete-slash.test.ts`,
//! `autocomplete-dollar.test.ts` and `dollar-skill-mentions.test.ts`.

use std::collections::BTreeSet;

use maho_tui::autocomplete::{AutocompleteProvider, CombinedAutocompleteProvider, CommandSpec};
use maho_tui::dollar_invocation_autocomplete::find_dollar_skill_mentions;

fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::with_description("model", "Select a model"),
        CommandSpec::with_description("reload", "Reload extensions"),
        CommandSpec::with_description("skill:debugging", "Debug runtime failures"),
        CommandSpec::with_description("skill:frontend", "Build web interfaces"),
    ]
}

fn values(provider: &mut CombinedAutocompleteProvider, line: &str) -> Vec<String> {
    provider
        .get_suggestions(&[line.to_string()], 0, line.len(), false)
        .map(|suggestions| suggestions.items.into_iter().map(|item| item.value).collect())
        .unwrap_or_default()
}

fn suggestions(provider: &mut CombinedAutocompleteProvider, line: &str) -> Option<(Vec<String>, String)> {
    provider
        .get_suggestions(&[line.to_string()], 0, line.len(), false)
        .map(|suggestions| {
            (
                suggestions.items.into_iter().map(|item| item.value).collect(),
                suggestions.prefix,
            )
        })
}

#[test]
fn ranks_longer_prefix_matches_before_shorter_commands_when_ambiguous() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("session", "Show session info and stats"),
            CommandSpec::with_description("sessions", "Peek at previous session transcripts in a HUD"),
        ],
        "/tmp",
        None,
    );

    assert_eq!(values(&mut provider, "/sessio"), vec!["sessions", "session"]);
}

#[test]
fn keeps_exact_slash_command_matches_before_longer_commands() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("session", "Show session info and stats"),
            CommandSpec::with_description("sessions", "Peek at previous session transcripts in a HUD"),
        ],
        "/tmp",
        None,
    );

    assert_eq!(values(&mut provider, "/session"), vec!["session", "sessions"]);
}

#[test]
fn keeps_skill_discovery_contextual_without_leaving_the_namespace_prefix_empty() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("model", "Select a model"),
            CommandSpec::with_description("skill:debugging", "Debug runtime failures"),
            CommandSpec::with_description("skill:frontend", "Build web interfaces"),
            CommandSpec::with_description("skill:ulw-plan", "Create an implementation plan"),
        ],
        "/tmp",
        None,
    );
    let all_skills = vec!["skill:debugging", "skill:frontend", "skill:ulw-plan"];

    assert_eq!(values(&mut provider, "/"), vec!["model"]);
    assert_eq!(values(&mut provider, "/skill"), vec!["skill:"]);
    assert_eq!(values(&mut provider, "/SKILL:"), all_skills);
    assert_eq!(values(&mut provider, "/skill:"), all_skills);
    assert_eq!(values(&mut provider, "/ul"), vec!["skill:ulw-plan"]);
}

#[test]
fn reopens_only_skill_suggestions_for_an_executable_second_leading_skill_command() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("model", "Select a model"),
            CommandSpec::with_description("skill:first", "First skill"),
            CommandSpec::with_description("skill:second", "Second skill"),
        ],
        "/tmp",
        None,
    );
    let line = "/skill:first /skill:se";

    let (items, prefix) = suggestions(&mut provider, line).expect("suggestions");
    assert_eq!(items, vec!["skill:second"]);
    assert_eq!(prefix, "/skill:se");

    let item = maho_tui::autocomplete::AutocompleteItem {
        value: "skill:second".to_string(),
        label: "skill:second".to_string(),
        description: None,
    };
    let completion = provider.apply_completion(&[line.to_string()], 0, line.len(), &item, &prefix);
    assert_eq!(completion.lines, vec!["/skill:first /skill:second "]);
    assert_eq!(completion.cursor_col, "/skill:first /skill:second ".len());
}

#[test]
fn does_not_suggest_skill_commands_outside_an_executable_leading_run() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("model", "Select a model"),
            CommandSpec::with_description("skill:first", "First skill"),
            CommandSpec::with_description("skill:second", "Second skill"),
        ],
        "/tmp",
        None,
    );

    assert!(provider
        .get_suggestions(&["prose /skill:se".to_string()], 0, 15, false)
        .is_none());
    assert!(provider
        .get_suggestions(&["/skill:missing /skill:se".to_string()], 0, 24, false)
        .is_none());
}

#[test]
fn groups_canonical_commands_before_skills_for_a_leading_dollar_trigger() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);

    let (items, prefix) = suggestions(&mut provider, "$").expect("suggestions");
    assert_eq!(items, vec!["/model", "/reload", "$debugging", "$frontend"]);
    assert_eq!(prefix, "$");
}

#[test]
fn filters_commands_and_skills_through_the_same_dollar_query() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);

    assert_eq!(values(&mut provider, "$rel"), vec!["/reload"]);
    assert_eq!(values(&mut provider, "$deb"), vec!["$debugging"]);
}

#[test]
fn inserts_canonical_slash_commands_and_bare_leading_dollar_skills() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);
    let command = suggestions(&mut provider, "$rel").expect("command");
    let skill = suggestions(&mut provider, "$deb").expect("skill");

    let command_item = maho_tui::autocomplete::AutocompleteItem {
        value: command.0[0].clone(),
        label: command.0[0].clone(),
        description: None,
    };
    let skill_item = maho_tui::autocomplete::AutocompleteItem {
        value: skill.0[0].clone(),
        label: skill.0[0].clone(),
        description: None,
    };

    let applied_command = provider.apply_completion(&["$rel".to_string()], 0, 4, &command_item, &command.1);
    assert_eq!(applied_command.lines, vec!["/reload "]);
    assert_eq!(applied_command.cursor_col, "/reload ".len());

    let applied_skill = provider.apply_completion(&["$deb".to_string()], 0, 4, &skill_item, &skill.1);
    assert_eq!(applied_skill.lines, vec!["$debugging "]);
    assert_eq!(applied_skill.cursor_col, "$debugging ".len());
}

#[test]
fn reopens_only_known_skills_for_a_second_leading_dollar_token() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);
    let line = "$debugging $front";

    let (items, prefix) = suggestions(&mut provider, line).expect("suggestions");
    assert_eq!(items, vec!["$frontend"]);
    assert_eq!(prefix, "$front");

    let item = maho_tui::autocomplete::AutocompleteItem {
        value: "$frontend".to_string(),
        label: "$frontend".to_string(),
        description: None,
    };
    let applied = provider.apply_completion(&[line.to_string()], 0, line.len(), &item, &prefix);
    assert_eq!(applied.lines, vec!["$debugging $frontend "]);
    assert_eq!(applied.cursor_col, "$debugging $frontend ".len());
}

#[test]
fn preserves_explicit_skill_namespace_chaining() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);
    let line = "$skill:debugging $front";

    let (items, prefix) = suggestions(&mut provider, line).expect("suggestions");
    assert_eq!(items, vec!["$frontend"]);
    assert_eq!(prefix, "$front");

    let item = maho_tui::autocomplete::AutocompleteItem {
        value: "$frontend".to_string(),
        label: "$frontend".to_string(),
        description: None,
    };
    let applied = provider.apply_completion(&[line.to_string()], 0, line.len(), &item, &prefix);
    assert_eq!(applied.lines, vec!["$skill:debugging $frontend "]);
    assert_eq!(applied.cursor_col, "$skill:debugging $frontend ".len());
}

#[test]
fn offers_skills_on_a_later_logical_editor_line() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![CommandSpec::with_description("skill:debugging", "Debug runtime failures")],
        "/tmp",
        None,
    );
    let lines = vec!["first line".to_string(), "text $".to_string()];
    let cursor_col = lines[1].len();

    let result = provider
        .get_suggestions(&lines, 1, cursor_col, false)
        .expect("suggestions");
    assert_eq!(
        result.items.iter().map(|item| item.value.clone()).collect::<Vec<_>>(),
        vec!["$debugging"]
    );
    assert_eq!(result.prefix, "$");

    let item = result.items[0].clone();
    let applied = provider.apply_completion(&lines, 1, cursor_col, &item, &result.prefix);
    assert_eq!(applied.lines, vec!["first line", "text $debugging "]);
    assert_eq!(applied.cursor_line, 1);
    assert_eq!(applied.cursor_col, "text $debugging ".len());
}

#[test]
fn offers_partial_skills_after_ordinary_prompt_text() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);

    assert_eq!(values(&mut provider, "explain $deb"), vec!["$debugging"]);
    assert!(provider
        .get_suggestions(&["$deb".to_string()], 1, 4, false)
        .is_none());
}

#[test]
fn offers_skills_only_once_the_dollar_token_is_not_the_first_token() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);

    assert_eq!(
        values(&mut provider, "explain $"),
        vec!["$debugging", "$frontend"]
    );
}

#[test]
fn reopens_for_a_later_dollar_token_after_an_earlier_mention_or_an_unknown_token() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);
    let line = "fix $debugging then $fro";

    let (items, prefix) = suggestions(&mut provider, line).expect("suggestions");
    assert_eq!(items, vec!["$frontend"]);
    assert_eq!(prefix, "$fro");

    let item = maho_tui::autocomplete::AutocompleteItem {
        value: "$frontend".to_string(),
        label: "$frontend".to_string(),
        description: None,
    };
    let applied = provider.apply_completion(&[line.to_string()], 0, line.len(), &item, &prefix);
    assert_eq!(applied.lines, vec!["fix $debugging then $frontend "]);
    assert_eq!(applied.cursor_col, "fix $debugging then $frontend ".len());

    assert_eq!(values(&mut provider, "$missing $deb"), vec!["$debugging"]);
}

#[test]
fn closes_once_the_token_is_exactly_a_known_skill_so_enter_submits() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);

    for line in ["$debugging", "run $debugging", "$frontend $debugging"] {
        assert!(
            provider
                .get_suggestions(&[line.to_string()], 0, line.len(), false)
                .is_none(),
            "{line}"
        );
    }
}

#[test]
fn leaves_shell_variables_and_positional_parameters_literal() {
    let mut provider = CombinedAutocompleteProvider::new(commands(), "/tmp", None);

    for line in ["echo $HOME", "echo $1"] {
        assert!(
            provider
                .get_suggestions(&[line.to_string()], 0, line.len(), false)
                .is_none(),
            "{line}"
        );
    }
}

fn known_skills() -> BTreeSet<String> {
    ["debugging".to_string(), "frontend".to_string()].into_iter().collect()
}

#[test]
fn finds_every_boundary_token_that_names_a_known_skill() {
    let mentions = find_dollar_skill_mentions("fix $debugging then $frontend please", &known_skills());

    assert_eq!(mentions.len(), 2);
    assert_eq!(mentions[0].start, 4);
    assert_eq!(mentions[0].end, 14);
    assert_eq!(mentions[0].name, "debugging");
    assert_eq!(mentions[1].start, 20);
    assert_eq!(mentions[1].end, 29);
    assert_eq!(mentions[1].name, "frontend");
}

#[test]
fn resolves_the_explicit_skill_namespace_and_keeps_the_whole_token() {
    let mentions = find_dollar_skill_mentions("$skill:debugging go", &known_skills());

    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].start, 0);
    assert_eq!(mentions[0].end, 16);
    assert_eq!(mentions[0].name, "debugging");
}

#[test]
fn leaves_unknown_shell_style_positional_and_embedded_dollars_plain() {
    let mentions = find_dollar_skill_mentions(
        "echo $HOME $1 $missing a$debugging $debugging-x",
        &known_skills(),
    );

    assert!(mentions.is_empty());
}

#[test]
fn does_not_mention_anything_when_no_skills_are_loaded() {
    assert!(find_dollar_skill_mentions("$debugging", &BTreeSet::new()).is_empty());
}

#[test]
fn maps_the_command_lists_skill_entries_onto_mention_ranges() {
    let provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("model", "Select a model"),
            CommandSpec::with_description("skill:debugging", "Debug runtime failures"),
        ],
        "/tmp",
        None,
    );

    let ranges = provider.get_mention_ranges("$model $debugging");
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].start, 7);
    assert_eq!(ranges[0].end, 17);
}

// ---------- autocomplete.test.ts: path-prefix extraction and directory completion ----------

fn force_suggestions(
    provider: &mut CombinedAutocompleteProvider,
    lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
) -> Option<(Vec<String>, String)> {
    provider
        .get_suggestions(lines, cursor_line, cursor_col, true)
        .map(|suggestions| {
            (
                suggestions.items.into_iter().map(|item| item.value).collect(),
                suggestions.prefix,
            )
        })
}

#[test]
fn extracts_slash_from_hey_slash_when_forced() {
    let mut provider = CombinedAutocompleteProvider::new(vec![], "/tmp", None);
    let result = force_suggestions(&mut provider, &["hey /".to_string()], 0, 5);
    assert!(result.is_some(), "should return suggestions for root directory");
    assert_eq!(result.unwrap().1, "/");
}

#[test]
fn extracts_slash_a_from_slash_a_when_forced() {
    let mut provider = CombinedAutocompleteProvider::new(vec![], "/tmp", None);
    if let Some((_items, prefix)) = force_suggestions(&mut provider, &["/A".to_string()], 0, 2) {
        assert_eq!(prefix, "/A");
    }
}

#[test]
fn does_not_trigger_for_slash_commands() {
    let mut provider = CombinedAutocompleteProvider::new(vec![], "/tmp", None);
    assert!(provider
        .get_suggestions(&["/model".to_string()], 0, 6, true)
        .is_none());
}

#[test]
fn triggers_for_absolute_paths_after_slash_command_argument() {
    let mut provider = CombinedAutocompleteProvider::new(vec![], "/tmp", None);
    let result = force_suggestions(&mut provider, &["/command /".to_string()], 0, 10);
    assert!(result.is_some(), "should trigger for absolute paths in command arguments");
    assert_eq!(result.unwrap().1, "/");
}

fn write_file(base: &std::path::Path, relative: &str, contents: &str) {
    let path = base.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write");
}

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn preserves_dot_slash_prefix_when_completing_paths() {
    let dir = temp_dir();
    write_file(dir.path(), "update.sh", "#!/bin/bash");
    write_file(dir.path(), "utils.ts", "export {};");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let (values, _prefix) =
        force_suggestions(&mut provider, &["./up".to_string()], 0, 4).expect("suggestions");
    assert!(values.contains(&"./update.sh".to_string()), "{values:?}");
}

#[test]
fn preserves_dot_slash_prefix_for_directory_completions() {
    let dir = temp_dir();
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
    write_file(dir.path(), "src/index.ts", "export {};");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let (values, _prefix) =
        force_suggestions(&mut provider, &["./sr".to_string()], 0, 4).expect("suggestions");
    assert!(values.contains(&"./src/".to_string()), "{values:?}");
}

#[test]
fn quotes_paths_with_spaces_for_direct_completion() {
    let dir = temp_dir();
    write_file(dir.path(), "my folder/test.txt", "content");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let (values, _prefix) =
        force_suggestions(&mut provider, &["my".to_string()], 0, 2).expect("suggestions");
    assert!(values.contains(&"\"my folder/\"".to_string()), "{values:?}");
}

#[test]
fn continues_completion_inside_quoted_paths() {
    let dir = temp_dir();
    write_file(dir.path(), "my folder/test.txt", "content");
    write_file(dir.path(), "my folder/other.txt", "content");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let line = "\"my folder/\"";
    let (values, _prefix) =
        force_suggestions(&mut provider, &[line.to_string()], 0, line.len() - 1).expect("suggestions");
    assert!(values.contains(&"\"my folder/test.txt\"".to_string()), "{values:?}");
    assert!(values.contains(&"\"my folder/other.txt\"".to_string()), "{values:?}");
}

#[test]
fn applies_quoted_completion_without_duplicating_the_closing_quote() {
    let dir = temp_dir();
    write_file(dir.path(), "my folder/test.txt", "content");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let line = "\"my folder/te\"";
    let cursor_col = line.len() - 1;
    let result = provider
        .get_suggestions(&[line.to_string()], 0, cursor_col, true)
        .expect("suggestions");
    let item = result
        .items
        .iter()
        .find(|item| item.value == "\"my folder/test.txt\"")
        .expect("test.txt suggestion");

    let applied = provider.apply_completion(&[line.to_string()], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "\"my folder/test.txt\"");
}
