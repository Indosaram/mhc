//! Port of senpi `packages/tui/test/autocomplete-slash.test.ts`,
//! `autocomplete-dollar.test.ts`, `dollar-skill-mentions.test.ts` and `autocomplete.test.ts`.

use std::collections::BTreeSet;

use maho_tui::autocomplete::{
    AutocompleteProvider, AutocompleteSignal, CombinedAutocompleteProvider, CommandSpec,
    SuggestionRequest,
};
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
        .map(|suggestions| {
            suggestions
                .items
                .into_iter()
                .map(|item| item.value)
                .collect()
        })
        .unwrap_or_default()
}

fn suggestions(
    provider: &mut CombinedAutocompleteProvider,
    line: &str,
) -> Option<(Vec<String>, String)> {
    provider
        .get_suggestions(&[line.to_string()], 0, line.len(), false)
        .map(|suggestions| {
            (
                suggestions
                    .items
                    .into_iter()
                    .map(|item| item.value)
                    .collect(),
                suggestions.prefix,
            )
        })
}

#[test]
fn ranks_longer_prefix_matches_before_shorter_commands_when_ambiguous() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("session", "Show session info and stats"),
            CommandSpec::with_description(
                "sessions",
                "Peek at previous session transcripts in a HUD",
            ),
        ],
        "/tmp",
        None,
    );

    assert_eq!(
        values(&mut provider, "/sessio"),
        vec!["sessions", "session"]
    );
}

#[test]
fn keeps_exact_slash_command_matches_before_longer_commands() {
    let mut provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("session", "Show session info and stats"),
            CommandSpec::with_description(
                "sessions",
                "Peek at previous session transcripts in a HUD",
            ),
        ],
        "/tmp",
        None,
    );

    assert_eq!(
        values(&mut provider, "/session"),
        vec!["session", "sessions"]
    );
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

    assert!(
        provider
            .get_suggestions(&["prose /skill:se".to_string()], 0, 15, false)
            .is_none()
    );
    assert!(
        provider
            .get_suggestions(&["/skill:missing /skill:se".to_string()], 0, 24, false)
            .is_none()
    );
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

    let applied_command =
        provider.apply_completion(&["$rel".to_string()], 0, 4, &command_item, &command.1);
    assert_eq!(applied_command.lines, vec!["/reload "]);
    assert_eq!(applied_command.cursor_col, "/reload ".len());

    let applied_skill =
        provider.apply_completion(&["$deb".to_string()], 0, 4, &skill_item, &skill.1);
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
        vec![CommandSpec::with_description(
            "skill:debugging",
            "Debug runtime failures",
        )],
        "/tmp",
        None,
    );
    let lines = vec!["first line".to_string(), "text $".to_string()];
    let cursor_col = lines[1].len();

    let result = provider
        .get_suggestions(&lines, 1, cursor_col, false)
        .expect("suggestions");
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.value.clone())
            .collect::<Vec<_>>(),
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
    assert!(
        provider
            .get_suggestions(&["$deb".to_string()], 1, 4, false)
            .is_none()
    );
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
    ["debugging".to_string(), "frontend".to_string()]
        .into_iter()
        .collect()
}

#[test]
fn finds_every_boundary_token_that_names_a_known_skill() {
    let mentions =
        find_dollar_skill_mentions("fix $debugging then $frontend please", &known_skills());

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
                suggestions
                    .items
                    .into_iter()
                    .map(|item| item.value)
                    .collect(),
                suggestions.prefix,
            )
        })
}

#[test]
fn extracts_slash_from_hey_slash_when_forced() {
    let mut provider = CombinedAutocompleteProvider::new(vec![], "/tmp", None);
    let result = force_suggestions(&mut provider, &["hey /".to_string()], 0, 5);
    assert!(
        result.is_some(),
        "should return suggestions for root directory"
    );
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
    assert!(
        provider
            .get_suggestions(&["/model".to_string()], 0, 6, true)
            .is_none()
    );
}

#[test]
fn triggers_for_absolute_paths_after_slash_command_argument() {
    let mut provider = CombinedAutocompleteProvider::new(vec![], "/tmp", None);
    let result = force_suggestions(&mut provider, &["/command /".to_string()], 0, 10);
    assert!(
        result.is_some(),
        "should trigger for absolute paths in command arguments"
    );
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
        force_suggestions(&mut provider, &[line.to_string()], 0, line.len() - 1)
            .expect("suggestions");
    assert!(
        values.contains(&"\"my folder/test.txt\"".to_string()),
        "{values:?}"
    );
    assert!(
        values.contains(&"\"my folder/other.txt\"".to_string()),
        "{values:?}"
    );
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

    let applied =
        provider.apply_completion(&[line.to_string()], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "\"my folder/test.txt\"");
}

#[test]
fn resolves_at_file_requests_off_thread_and_slash_requests_immediately() {
    let mut provider =
        CombinedAutocompleteProvider::new(commands(), "/tmp", Some("fd".to_string()));
    let signal = AutocompleteSignal::new();
    // Aborted before the worker runs, so no fd process is ever spawned.
    signal.abort();
    match provider.request_suggestions(&["@src".to_string()], 0, 4, false, &signal) {
        SuggestionRequest::Pending(receiver) => {
            let answer = receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("worker answers");
            assert!(answer.is_none(), "an aborted walk yields no suggestions");
        }
        SuggestionRequest::Ready(_) => panic!("the @ file search must not block the caller"),
    }
    assert!(matches!(
        provider.request_suggestions(
            &["/mod".to_string()],
            0,
            4,
            false,
            &AutocompleteSignal::new()
        ),
        SuggestionRequest::Ready(Some(_))
    ));
}

// ---------- autocomplete.test.ts: fd @ file suggestions ----------
//
// senpi skips this group when `fd` is not on PATH (`{ skip: !isFdInstalled }`); these tests
// return early the same way.

fn fd_path() -> Option<String> {
    std::process::Command::new("fd")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|_| "fd".to_string())
}

struct FdFixture {
    root: tempfile::TempDir,
}

impl FdFixture {
    fn new() -> Self {
        let root = temp_dir();
        std::fs::create_dir_all(root.path().join("cwd")).expect("mkdir cwd");
        std::fs::create_dir_all(root.path().join("outside")).expect("mkdir outside");
        Self { root }
    }

    fn base(&self) -> std::path::PathBuf {
        self.root.path().join("cwd")
    }

    fn outside(&self) -> std::path::PathBuf {
        self.root.path().join("outside")
    }

    fn provider(&self, fd: &str) -> CombinedAutocompleteProvider {
        CombinedAutocompleteProvider::new(
            vec![],
            &self.base().to_string_lossy(),
            Some(fd.to_string()),
        )
    }
}

fn mkdirs(base: &std::path::Path, dirs: &[&str]) {
    for dir in dirs {
        std::fs::create_dir_all(base.join(dir)).expect("mkdir");
    }
}

fn at_values(provider: &mut CombinedAutocompleteProvider, line: &str) -> Vec<String> {
    values(provider, line)
}

#[test]
fn returns_all_files_and_folders_for_empty_at_query() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    mkdirs(&fixture.base(), &["src"]);
    write_file(&fixture.base(), "README.md", "readme");

    let mut values = at_values(&mut fixture.provider(&fd), "@");
    values.sort();
    assert_eq!(values, vec!["@README.md", "@src/"]);
}

#[test]
fn matches_file_with_extension_in_query() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "file.txt", "content");

    let values = at_values(&mut fixture.provider(&fd), "@file.txt");
    assert!(values.contains(&"@file.txt".to_string()), "{values:?}");
}

#[test]
fn at_filters_are_case_insensitive() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    mkdirs(&fixture.base(), &["src"]);
    write_file(&fixture.base(), "README.md", "readme");

    assert_eq!(
        at_values(&mut fixture.provider(&fd), "@re"),
        vec!["@README.md"]
    );
}

#[test]
fn ranks_directories_before_files() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    mkdirs(&fixture.base(), &["src"]);
    write_file(&fixture.base(), "src.txt", "text");

    let values = at_values(&mut fixture.provider(&fd), "@src");
    assert_eq!(values.first().map(String::as_str), Some("@src/"));
    assert!(values.contains(&"@src.txt".to_string()), "{values:?}");
}

#[test]
fn returns_nested_file_paths() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "src/index.ts", "export {};\n");

    let values = at_values(&mut fixture.provider(&fd), "@index");
    assert!(values.contains(&"@src/index.ts".to_string()), "{values:?}");
}

#[test]
fn matches_deeply_nested_paths() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(
        &fixture.base(),
        "packages/tui/src/autocomplete.ts",
        "export {};",
    );
    write_file(
        &fixture.base(),
        "packages/ai/src/autocomplete.ts",
        "export {};",
    );

    let values = at_values(&mut fixture.provider(&fd), "@tui/src/auto");
    assert!(
        values.contains(&"@packages/tui/src/autocomplete.ts".to_string()),
        "{values:?}"
    );
    assert!(
        !values.contains(&"@packages/ai/src/autocomplete.ts".to_string()),
        "{values:?}"
    );
}

#[test]
fn matches_directory_in_middle_of_path_with_full_path() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "src/components/Button.tsx", "export {};");
    write_file(&fixture.base(), "src/utils/helpers.ts", "export {};");

    let values = at_values(&mut fixture.provider(&fd), "@components/");
    assert!(
        values.contains(&"@src/components/Button.tsx".to_string()),
        "{values:?}"
    );
    assert!(
        !values.contains(&"@src/utils/helpers.ts".to_string()),
        "{values:?}"
    );
}

#[test]
fn scopes_fuzzy_search_to_relative_directories_and_searches_recursively() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.outside(), "nested/alpha.ts", "export {};");
    write_file(
        &fixture.outside(),
        "nested/deeper/also-alpha.ts",
        "export {};",
    );
    write_file(&fixture.outside(), "nested/deeper/zzz.ts", "export {};");

    let values = at_values(&mut fixture.provider(&fd), "@../outside/a");
    assert!(
        values.contains(&"@../outside/nested/alpha.ts".to_string()),
        "{values:?}"
    );
    assert!(
        values.contains(&"@../outside/nested/deeper/also-alpha.ts".to_string()),
        "{values:?}"
    );
    assert!(
        !values.contains(&"@../outside/nested/deeper/zzz.ts".to_string()),
        "{values:?}"
    );
}

#[test]
fn ranks_shallower_same_score_at_matches_before_deeper_matches() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    mkdirs(
        &fixture.base(),
        &[
            "scope/aaa/venv/lib/python3.12/site-packages/pkg/core/profile",
            "scope/projects",
        ],
    );

    let values = at_values(&mut fixture.provider(&fd), "@scope/pro");
    assert_eq!(values.first().map(String::as_str), Some("@scope/projects/"));
    assert!(
        values.contains(
            &"@scope/aaa/venv/lib/python3.12/site-packages/pkg/core/profile/".to_string()
        ),
        "{values:?}"
    );
}

#[test]
fn includes_scoped_direct_children_when_recursive_at_matches_are_flooded() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    mkdirs(&fixture.base(), &["scope/projects"]);
    for index in 1..=250 {
        mkdirs(
            &fixture.base(),
            &[
                format!("scope/a{index:03}/venv/lib/python3.12/site-packages/pkg/core/profile")
                    .as_str(),
            ],
        );
    }

    let values = at_values(&mut fixture.provider(&fd), "@scope/pro");
    assert_eq!(values.first().map(String::as_str), Some("@scope/projects/"));
    assert!(
        values.iter().any(|value| value.contains("/profile/")),
        "deep fuzzy matches should follow the direct children: {values:?}"
    );
}

#[test]
fn includes_hidden_paths_but_excludes_dot_git() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    mkdirs(&fixture.base(), &[".pi", ".github", ".git"]);
    write_file(&fixture.base(), ".pi/config.json", "{}");
    write_file(&fixture.base(), ".github/workflows/ci.yml", "name: ci");
    write_file(&fixture.base(), ".git/config", "[core]");

    let values = at_values(&mut fixture.provider(&fd), "@");
    assert!(values.contains(&"@.pi/".to_string()), "{values:?}");
    assert!(values.contains(&"@.github/".to_string()), "{values:?}");
    assert!(
        !values
            .iter()
            .any(|value| value == "@.git" || value.starts_with("@.git/")),
        "{values:?}"
    );
}

#[cfg(unix)]
#[test]
fn follows_symlinked_directories_for_fuzzy_at_search() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "dir/some_file.txt", "real");
    write_file(&fixture.outside(), "some_file.txt", "symlinked");
    std::os::unix::fs::symlink("../outside", fixture.base().join("symlinked_dir"))
        .expect("symlink");

    let values = at_values(&mut fixture.provider(&fd), "@some");
    assert!(
        values.contains(&"@dir/some_file.txt".to_string()),
        "{values:?}"
    );
    assert!(
        values.contains(&"@symlinked_dir/some_file.txt".to_string()),
        "{values:?}"
    );
}

#[cfg(unix)]
#[test]
fn returns_symlinked_directories_when_matching_their_name() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.outside(), "nested/file.txt", "symlinked");
    std::os::unix::fs::symlink("../outside", fixture.base().join("symlinked_dir"))
        .expect("symlink");

    let values = at_values(&mut fixture.provider(&fd), "@symlinked");
    assert!(
        values.contains(&"@symlinked_dir/".to_string()),
        "{values:?}"
    );
}

#[cfg(unix)]
#[test]
fn returns_symlinked_files_without_requiring_type_l() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "original.txt", "content");
    std::os::unix::fs::symlink("original.txt", fixture.base().join("link.txt")).expect("symlink");

    let values = at_values(&mut fixture.provider(&fd), "@link");
    assert!(values.contains(&"@link.txt".to_string()), "{values:?}");
}

#[test]
fn returns_the_same_at_suggestions_when_the_cwd_path_contains_the_query() {
    let Some(fd) = fd_path() else { return };
    let root = temp_dir();
    let normal = root.path().join("cwd-normal");
    let query_in_path = root.path().join("cwd-plan-repro");
    for base in [&normal, &query_in_path] {
        mkdirs(
            base,
            &["packages/coding-agent/examples/extensions/plan-mode"],
        );
        write_file(
            base,
            "packages/coding-agent/examples/extensions/plan-mode/README.md",
            "readme",
        );
        write_file(base, "packages/tui/docs/plan.md", "plan");
    }
    let normalize = |base: &std::path::Path| -> Vec<String> {
        let mut provider =
            CombinedAutocompleteProvider::new(vec![], &base.to_string_lossy(), Some(fd.clone()));
        let mut entries: Vec<String> = provider
            .get_suggestions(&["@plan".to_string()], 0, 5, false)
            .map(|result| result.items)
            .unwrap_or_default()
            .into_iter()
            .map(|item| format!("{} :: {}", item.label, item.description.unwrap_or_default()))
            .collect();
        entries.sort();
        entries
    };

    let normal_entries = normalize(&normal);
    assert_eq!(normalize(&query_in_path), normal_entries);
    assert!(
        normal_entries.contains(
            &"plan-mode/ :: packages/coding-agent/examples/extensions/plan-mode".to_string()
        ),
        "{normal_entries:?}"
    );
    assert!(
        normal_entries.contains(&"plan.md :: packages/tui/docs/plan.md".to_string()),
        "{normal_entries:?}"
    );
}

#[test]
fn continues_autocomplete_inside_quoted_at_paths() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "my folder/test.txt", "content");
    write_file(&fixture.base(), "my folder/other.txt", "content");

    let line = "@\"my folder/\"";
    let values: Vec<String> = fixture
        .provider(&fd)
        .get_suggestions(&[line.to_string()], 0, line.len() - 1, false)
        .expect("suggestions for a quoted folder path")
        .items
        .into_iter()
        .map(|item| item.value)
        .collect();
    assert!(
        values.contains(&"@\"my folder/test.txt\"".to_string()),
        "{values:?}"
    );
    assert!(
        values.contains(&"@\"my folder/other.txt\"".to_string()),
        "{values:?}"
    );
}

#[test]
fn applies_quoted_at_completion_without_duplicating_closing_quote() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "my folder/test.txt", "content");
    let mut provider = fixture.provider(&fd);

    let line = "@\"my folder/te\"";
    let cursor_col = line.len() - 1;
    let result = provider
        .get_suggestions(&[line.to_string()], 0, cursor_col, false)
        .expect("suggestions for a quoted @ path");
    let item = result
        .items
        .iter()
        .find(|item| item.value == "@\"my folder/test.txt\"")
        .expect("test.txt suggestion");

    let applied =
        provider.apply_completion(&[line.to_string()], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "@\"my folder/test.txt\" ");
}

// ---------- autocomplete.test.ts: path completion without fd ----------

#[test]
fn completes_quoted_paths_after_opening_wrappers() {
    let dir = temp_dir();
    write_file(dir.path(), "my dir/main.ts", "x");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let line = "see (\"my dir/ma";
    let (values, prefix) =
        force_suggestions(&mut provider, &[line.to_string()], 0, line.len()).expect("suggestions");
    assert_eq!(prefix, "\"my dir/ma");
    assert_eq!(values, vec!["\"my dir/main.ts\""]);
}

#[test]
fn keeps_wrappers_that_are_closed_inside_the_path() {
    let dir = temp_dir();
    write_file(dir.path(), "[slug]/page.tsx", "x");
    write_file(dir.path(), "(group)/layout.tsx", "x");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    for (prefix, value) in [
        ("[slug]/pa", "[slug]/page.tsx"),
        ("(group)/la", "(group)/layout.tsx"),
        ("./[slug]/pa", "./[slug]/page.tsx"),
    ] {
        let line = format!("see {prefix}");
        let (values, actual_prefix) =
            force_suggestions(&mut provider, std::slice::from_ref(&line), 0, line.len()).expect("suggestions");
        assert_eq!(actual_prefix, prefix, "{line}");
        assert_eq!(values, vec![value.to_string()], "{line}");
    }
}

#[test]
fn preserves_cjk_characters_in_unprefixed_tab_completions() {
    let dir = temp_dir();
    write_file(dir.path(), "文档/说明.md", "text");
    let mut provider =
        CombinedAutocompleteProvider::new(vec![], &dir.path().to_string_lossy(), None);

    let line = "文档/说";
    let (values, prefix) =
        force_suggestions(&mut provider, &[line.to_string()], 0, line.len()).expect("suggestions");
    assert_eq!(prefix, line);
    assert_eq!(values, vec!["文档/说明.md"]);
}

// ---------- autocomplete.test.ts: dollar skill token boundaries ----------

fn commit_skill_provider() -> CombinedAutocompleteProvider {
    CombinedAutocompleteProvider::new(
        vec![CommandSpec::with_description(
            "skill:commit",
            "Commit changes",
        )],
        "/tmp",
        None,
    )
}

#[test]
fn offers_a_skill_after_ordinary_prompt_text_at_a_token_boundary() {
    let mut provider = commit_skill_provider();
    let line = "text $com";
    let result = provider
        .get_suggestions(&[line.to_string()], 0, line.len(), false)
        .expect("a dollar skill suggestion");
    assert_eq!(result.prefix, "$com");
    let values: Vec<&str> = result
        .items
        .iter()
        .map(|item| item.value.as_str())
        .collect();
    assert_eq!(values, vec!["$commit"]);

    let applied = provider.apply_completion(
        &[line.to_string()],
        0,
        line.len(),
        &result.items[0],
        &result.prefix,
    );
    assert_eq!(applied.lines[0], "text $commit ");
    assert_eq!(applied.cursor_col, "text $commit ".len());
}

#[test]
fn does_not_hijack_shell_like_dollar_syntax_or_complete_prose_words() {
    let mut provider = commit_skill_provider();
    assert!(
        provider
            .get_suggestions(&["text $HOME".to_string()], 0, 10, false)
            .is_none()
    );
    assert!(
        provider
            .get_suggestions(&["echo $1".to_string()], 0, 7, false)
            .is_none()
    );
    assert!(
        provider
            .get_suggestions(&["please use $commit".to_string()], 0, 18, false)
            .is_none()
    );
}

#[test]
fn does_not_interpret_email_addresses_or_at_after_ascii_or_cjk_letters_as_attachment_prefixes() {
    let Some(fd) = fd_path() else { return };
    let fixture = FdFixture::new();
    write_file(&fixture.base(), "README.md", "readme");
    write_file(&fixture.base(), "example.com", "text");
    let mut provider = fixture.provider(&fd);

    for before in [
        "user",
        "查看",
        "あ",
        "カ",
        "한",
        "ㄅ",
        "𠮷",
        "か\u{3099}",
        "禰\u{e0100}",
        "々",
        "Ａ",
    ] {
        for name in ["REA", "example.com"] {
            let line = format!("{before}@{name}");
            assert!(
                provider
                    .get_suggestions(std::slice::from_ref(&line), 0, line.len(), false)
                    .is_none(),
                "{line}"
            );
        }
    }
}
