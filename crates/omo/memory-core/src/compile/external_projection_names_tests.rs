use super::*;

const AWS_KEY_SHAPED: &str = concat!("AKIA", "ABCDEFGHIJKLMNOP");

fn files(names: &[&str]) -> Vec<NamedFile> {
    names
        .iter()
        .map(|name| NamedFile {
            name: (*name).to_string(),
            time: 0.0,
            files: 1,
        })
        .collect()
}

fn texts(names: &[&str]) -> Vec<String> {
    directory_items(&files(names))
        .into_iter()
        .map(|item| item.text)
        .collect()
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

fn range(count: usize, name: impl Fn(usize) -> String) -> Vec<String> {
    (0..count).map(name).collect()
}

fn refs(values: &[String]) -> Vec<&str> {
    values.iter().map(String::as_str).collect()
}

fn expect_items(case: &str, names: &[&str], expected: &[&str]) {
    assert_eq!(
        sorted(texts(names)),
        sorted(expected.iter().map(|s| (*s).to_string()).collect()),
        "{case}"
    );
}

#[test]
fn directory_items_name_classes() {
    expect_items(
        "hinted markdown name",
        &["release-checklist-notes.md"],
        &["release-checklist-notes"],
    );
    expect_items(
        "dotted markdown name",
        &["1password.com.md"],
        &["1password.com"],
    );
    expect_items(
        "non-markdown name",
        &[".dream-state.json"],
        &["\".dream-state.json\""],
    );
    expect_items(
        "lone dated name",
        &["quarterly-review-20261012.md"],
        &["quarterly-review-20261012"],
    );
    expect_items(
        "hinted names sharing a date",
        &["alpha-20261012.md", "beta-20261012.md", "gamma-20261013.md"],
        &["{alpha, beta}-20261012", "gamma-20261013"],
    );
    let months = range(12, |i| format!("2026-{:02}-p{}.md", 6 + (i % 5), i + 1));
    expect_items(
        "month chunks fold into one range",
        &refs(&months),
        &["2026-06-p1 … 2026-10-p10 (12 files)"],
    );
    let standups = range(5, |i| format!("standup-notes-2026102{i}-0{i}00-0{i}30z.md"));
    expect_items(
        "same-prefix timestamped names fold",
        &refs(&standups),
        &["standup-notes-20261020-0000-0030z … standup-notes-20261024-0400-0430z (5 files)"],
    );
    expect_items(
        "generated-id names fold into one id entry",
        &[
            "0b6e3c1a-2d4f-4e8a-9b1c-3d5e7f9a1b2c.md",
            "1c7f4d2b-3e5a-4f9b-8c2d-4e6f8a0b2c3d.md",
            "2d8a5e3c-4f6b-4a0c-9d3e-5f7a9b1c3d4e.md",
        ],
        &["<id> (3 files)"],
    );
    expect_items(
        "two hint-free names stay listed",
        &["reflection-20261017.md", "reflection-20261018.md"],
        &["reflection-20261017", "reflection-20261018"],
    );
    expect_items(
        "a pattern that is not shorter stays listed",
        &["2026-06.md", "2026-07.md", "2026-08.md"],
        &["2026-06", "2026-07", "2026-08"],
    );
    expect_items(
        "a word after the date is a hint",
        &[
            "standup-notes-20261020-afternoon.md",
            "standup-notes-20261021-evening.md",
            "standup-notes-20261022-late.md",
        ],
        &[
            "standup-notes-20261020-afternoon",
            "standup-notes-20261021-evening",
            "standup-notes-20261022-late",
        ],
    );
    expect_items(
        "names ending in digits are hints",
        &[
            "project2024.md",
            "gpt6.md",
            "GitHubActions2024.md",
            "iPhone15Pro.md",
        ],
        &["GitHubActions2024", "gpt6", "iPhone15Pro", "project2024"],
    );
    expect_items(
        "a leading date with a topic is a hint",
        &[
            "20261016-duty-corrections.md",
            "20261017-duty-handoff.md",
            "20261018-duty-ledger.md",
        ],
        &[
            "20261016-duty-corrections",
            "20261017-duty-handoff",
            "20261018-duty-ledger",
        ],
    );
    let secret = format!("{AWS_KEY_SHAPED}-20261012.md");
    expect_items(
        "secret-like names are masked before grouping",
        &[secret.as_str(), "plain-20261012.md"],
        &["{***, plain}-20261012"],
    );
}

#[test]
fn every_file_is_accounted_for_after_folding_and_grouping() {
    let mut names = range(4, |i| format!("2026-0{}-p1.md", i + 1));
    names.extend(["a-20261012.md", "b-20261012.md", "note.md"].map(String::from));
    let items = directory_items(&files(&refs(&names)));
    assert_eq!(
        items.iter().map(|item| item.files).sum::<usize>(),
        names.len()
    );
}

#[test]
fn items_are_newest_first_and_a_group_takes_its_newest_members_time() {
    let listed = [
        ("old.md", 1.0),
        ("a-20261012.md", 2.0),
        ("b-20261012.md", 9.0),
        ("mid.md", 5.0),
    ]
    .map(|(name, time)| NamedFile {
        name: name.into(),
        time,
        files: 1,
    });
    let items: Vec<String> = directory_items(&listed)
        .into_iter()
        .map(|item| item.text)
        .collect();
    assert_eq!(items, ["{b, a}-20261012", "mid", "old"]);
}

#[test]
fn generated_ids_are_judged_one_name_at_a_time() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "a UUID",
            &["0b6e3c1a-2d4f-4e8a-9b1c-3d5e7f9a1b2c"],
            &["0b6e3c1a-2d4f-4e8a-9b1c-3d5e7f9a1b2c"],
        ),
        (
            "hex digests of 16 or more characters",
            &["4bea7ccd8f2e19a0", "4bea7ccd", "deadbeefcafe2026"],
            &["4bea7ccd8f2e19a0"],
        ),
        (
            "same-length random tokens never pull a word-like one in",
            &[
                "Y2Gw9vTZ9pnH2K5a",
                "87U3F2ZseCw7bLMz",
                "wcjyAab9yeccko3U",
                "design-notes",
            ],
            &["Y2Gw9vTZ9pnH2K5a", "87U3F2ZseCw7bLMz"],
        ),
        (
            "two word-like random tokens are not a family",
            &["wcjyAab9yeccko3U", "XpTHKRroIBYxc5em"],
            &[],
        ),
        (
            "words ending in digits",
            &["project2024", "iPhone15Pro", "OAuth2Client", "1password"],
            &[],
        ),
        (
            "same-length lowercase words",
            &["agentportal", "chatgptpros", "dependencies"],
            &[],
        ),
    ];
    for (name, names, ids) in cases {
        let found = sorted(generated_ids(names).into_iter().collect());
        let expected = sorted(ids.iter().map(|s| (*s).to_string()).collect());
        assert_eq!(found, expected, "{name}");
    }
}
