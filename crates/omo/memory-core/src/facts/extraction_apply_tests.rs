//! Git-backed ports of extraction.test (apply), extraction-person-aliases,
//! extraction-person-lifecycle and extraction-routing-gate.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::facts::extraction::{
    ApplyFactsBatchOptions, ApplyFactsBatchResult, FactsBatch, FactsExtractionRecord,
    FactsPersonReference, apply_facts_batch,
};
use crate::facts::person_routing::{FactsAliasTie, FactsPeopleRouting};
use crate::git::{
    GitCommitAuthor, GitExec, GitExecOptions, GitExecResult, GitLogOptions, GitMemoryRepo,
    GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions, system_git_exec,
};
use crate::memfs::frontmatter::{MemoryFrontmatter, parse_memory_file, render_memory_file};
use crate::people::format::{ObservationEntry, PeopleLimits, parse_people_card};
use crate::seeds::seeds::build_default_seed_files;

const PEOPLE: FactsPeopleRouting = FactsPeopleRouting {
    enabled: true,
    max_entries: 40,
    max_entry_chars: 200,
};
const LIMITS: PeopleLimits = PeopleLimits {
    max_entries: 40,
    max_entry_chars: 200,
};

fn author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "facts-agent".to_string(),
        author_name: "Facts Extractor".to_string(),
        author_email: None,
    }
}

fn fixture_with(exec: Option<Arc<dyn GitExec>>) -> (TempDir, PathBuf, GitMemoryRepo) {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().canonicalize().expect("canonical");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        exec,
        ..GitMemoryRepoOptions::new(&dir, "facts-agent")
    })
    .expect("open repo");
    let seed_files = build_default_seed_files()
        .into_iter()
        .map(|s| GitSeedFile {
            relative_path: s.relative_path,
            content: s.content,
        })
        .collect();
    repo.init(InitializeGitRepoOptions {
        seed_files,
        ..Default::default()
    })
    .expect("init");
    (tmp, dir, repo)
}

fn fixture() -> (TempDir, PathBuf, GitMemoryRepo) {
    fixture_with(None)
}

fn person_record(name: &str, aliases: &[&str], text: &str, date: &str) -> FactsExtractionRecord {
    FactsExtractionRecord::Person {
        person: FactsPersonReference {
            name: name.to_string(),
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
        },
        text: text.to_string(),
        date: date.to_string(),
    }
}

fn project(text: &str) -> FactsExtractionRecord {
    FactsExtractionRecord::Project {
        text: text.to_string(),
        date: "2026-08-10".to_string(),
    }
}

fn batch(id: &str, records: Vec<FactsExtractionRecord>) -> FactsBatch {
    FactsBatch {
        batch_id: id.to_string(),
        records,
    }
}

fn with_people<'a>() -> ApplyFactsBatchOptions<'a> {
    ApplyFactsBatchOptions {
        people: Some(PEOPLE),
        ..Default::default()
    }
}

fn frontmatter(
    description: &str,
    kind: Option<&str>,
    aliases: Option<&[&str]>,
) -> MemoryFrontmatter {
    MemoryFrontmatter {
        description: description.to_string(),
        read_only: None,
        kind: kind.map(str::to_string),
        aliases: aliases.map(|a| a.iter().map(|s| (*s).to_string()).collect()),
    }
}

fn write_and_commit(repo: &GitMemoryRepo, rel: &str, content: &str, message: &str) {
    let full = repo.dir.join(rel);
    std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
    std::fs::write(full, content).expect("write");
    repo.commit_write(&[rel], message, &author())
        .expect("commit");
}

fn commit_person_card(repo: &GitMemoryRepo, slug: &str, name: &str, aliases: &[&str]) {
    let content = render_memory_file(
        &frontmatter(&format!("Person - {name}"), Some("person"), Some(aliases)),
        "",
    )
    .expect("render");
    write_and_commit(
        repo,
        &format!("people/{slug}/card.md"),
        &content,
        &format!("test: seed {slug} card"),
    );
}

fn commit_observations(repo: &GitMemoryRepo, slug: &str, name: &str, body: &str) {
    let content = render_memory_file(
        &frontmatter(&format!("Observations - {name}"), None, None),
        body,
    )
    .expect("render");
    write_and_commit(
        repo,
        &format!("people/{slug}/observations.md"),
        &content,
        &format!("test: seed {slug} observations"),
    );
}

fn explicit_entries(dir: &Path, slug: &str) -> Vec<ObservationEntry> {
    let raw = std::fs::read_to_string(dir.join("people").join(slug).join("observations.md"))
        .expect("observations");
    let memory = parse_memory_file(&raw).expect("parse");
    let parsed = parse_people_card(&memory.body, LIMITS);
    assert_eq!(parsed.diagnostics, Vec::<String>::new());
    parsed
        .card
        .observations
        .unwrap_or_default()
        .into_iter()
        .find(|g| g.section == "Explicit")
        .map(|g| g.entries)
        .unwrap_or_default()
}

fn contents(entries: &[ObservationEntry]) -> Vec<String> {
    entries.iter().map(|e| e.content.clone()).collect()
}

fn people_dirs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("people"))
        .expect("people dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn monthly_notes_body(dir: &Path) -> String {
    let raw = std::fs::read_to_string(dir.join("notes/facts/2026-08.md")).expect("notes");
    parse_memory_file(&raw).expect("parse").body
}

// --- extraction.test.ts (atomic application)

#[test]
fn given_project_and_person_records_without_people_policy_when_applied_then_one_notes_commit_preserves_texts_and_trailers()
 {
    let (_tmp, dir, repo) = fixture();

    let result = apply_facts_batch(
        &repo,
        batch(
            "11111111-1111-4111-8111-111111111111",
            vec![
                project("The project uses Bun."),
                person_record(
                    "Mina",
                    &["Min"],
                    "Mina prefers concise reviews.",
                    "2026-08-10",
                ),
            ],
        ),
        &author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect("apply");

    let sha = result.sha().expect("committed").to_string();
    let body = monthly_notes_body(&dir);
    assert!(
        body.contains("- [2026-08-10] The project uses Bun."),
        "{body}"
    );
    assert!(
        body.contains("- [2026-08-10] Mina prefers concise reviews."),
        "{body}"
    );
    let log = repo
        .log(Some(&GitLogOptions {
            range: Some("HEAD~1..HEAD".to_string()),
            ..Default::default()
        }))
        .expect("log");
    let commit = log.first().expect("commit");
    assert_eq!(commit.sha, sha);
    assert_eq!(commit.subject, "chore(facts): extract 2 facts");
    assert_eq!(
        commit.trailers.get("Generated-By").map(String::as_str),
        Some("facts-extractor")
    );
    assert_eq!(
        commit.trailers.get("Omo-Writer").map(String::as_str),
        Some("facts-extractor")
    );
    assert_eq!(
        commit.trailers.get("Omo-Facts-Batch").map(String::as_str),
        Some("11111111-1111-4111-8111-111111111111")
    );
}

#[test]
fn given_zero_applicable_records_when_applied_then_no_commit_is_attempted() {
    let (_tmp, _dir, repo) = fixture();
    let before = repo.head().expect("head");

    let result = apply_facts_batch(
        &repo,
        batch("22222222-2222-4222-8222-222222222222", vec![]),
        &author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect("apply");

    assert_eq!(
        result,
        ApplyFactsBatchResult::NoFacts {
            affected_paths: vec![]
        }
    );
    assert_eq!(repo.head().expect("head"), before);
}

struct CommitRejectingExec {
    base: Arc<dyn GitExec>,
    reject: Arc<AtomicBool>,
}

impl GitExec for CommitRejectingExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        if self.reject.load(Ordering::SeqCst) && argv.iter().any(|a| a == "commit") {
            return Ok(GitExecResult {
                code: 1,
                stdout: String::new(),
                stderr: "injected commit failure".to_string(),
            });
        }
        self.base.run(argv, options)
    }
}

#[test]
fn given_git_commit_fails_after_staging_when_batch_aborts_then_index_and_worktree_are_restored() {
    let reject = Arc::new(AtomicBool::new(false));
    let exec: Arc<dyn GitExec> = Arc::new(CommitRejectingExec {
        base: system_git_exec(),
        reject: Arc::clone(&reject),
    });
    let (_tmp, dir, repo) = fixture_with(Some(exec));
    reject.store(true, Ordering::SeqCst);

    let err = apply_facts_batch(
        &repo,
        batch(
            "33333333-3333-4333-8333-333333333333",
            vec![project("The project uses Bun.")],
        ),
        &author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect_err("commit must fail");

    assert!(err.to_string().contains("injected commit failure"), "{err}");
    assert_eq!(repo.status(&[] as &[&str]).expect("status"), "");
    assert!(!dir.join("notes/facts/2026-08.md").exists());
}

// --- extraction-person-aliases.test.ts

#[test]
fn given_existing_card_with_known_alias_when_fact_names_alias_then_it_lands_in_card_ledger_without_new_directory()
 {
    let (_tmp, dir, repo) = fixture();
    commit_person_card(&repo, "mina", "Mina", &["Min"]);

    let result = apply_facts_batch(
        &repo,
        batch(
            "44444444-4444-4444-8444-444444444444",
            vec![person_record(
                "Min",
                &[],
                "Min is reviewing the memory plan.",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    assert_eq!(result.outcome(), "committed");
    let entries = explicit_entries(&dir, "mina");
    assert_eq!(
        contents(&entries),
        vec!["Min is reviewing the memory plan."]
    );
    assert_eq!(entries[0].date, "2026-08-10");
    assert_eq!(entries[0].n, None);
    assert_eq!(people_dirs(&dir), vec!["mina"]);
}

#[test]
fn given_two_cards_whose_aliases_both_match_when_resolved_then_longest_alias_wins() {
    let (_tmp, dir, repo) = fixture();
    commit_person_card(&repo, "mina-kim", "Mina Kim", &["Min"]);
    commit_person_card(&repo, "mina-lee", "Mina Lee", &["Mina"]);

    apply_facts_batch(
        &repo,
        batch(
            "55555555-5555-4555-8555-555555555555",
            vec![person_record(
                "Mina",
                &["Min"],
                "Mina reviewed the plan.",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    assert_eq!(
        contents(&explicit_entries(&dir, "mina-lee")),
        vec!["Mina reviewed the plan."]
    );
    assert!(!dir.join("people/mina-kim/observations.md").exists());
}

#[test]
fn given_two_cards_with_equal_length_aliases_when_resolved_then_smallest_slug_wins_and_tie_is_logged()
 {
    let (_tmp, dir, repo) = fixture();
    commit_person_card(&repo, "bbb-min", "Min Bbb", &["Min"]);
    commit_person_card(&repo, "aaa-min", "Min Aaa", &["Min"]);
    let mut ties: Vec<FactsAliasTie> = Vec::new();
    let mut record_tie = |tie: &FactsAliasTie| ties.push(tie.clone());

    apply_facts_batch(
        &repo,
        batch(
            "66666666-6666-4666-8666-666666666666",
            vec![person_record(
                "Min",
                &[],
                "Min joined the platform guild.",
                "2026-08-10",
            )],
        ),
        &author(),
        ApplyFactsBatchOptions {
            people: Some(PEOPLE),
            on_alias_tie: Some(&mut record_tie),
            publish_recovery: None,
        },
    )
    .expect("apply");

    assert_eq!(
        contents(&explicit_entries(&dir, "aaa-min")),
        vec!["Min joined the platform guild."]
    );
    assert_eq!(
        ties,
        vec![FactsAliasTie {
            alias: "Min".to_string(),
            slugs: vec!["aaa-min".to_string(), "bbb-min".to_string()],
            chosen: "aaa-min".to_string()
        }]
    );
}

#[test]
fn given_primary_human_card_carries_alias_when_fact_names_it_then_it_routes_to_human_ledger_without_new_card()
 {
    let (_tmp, dir, repo) = fixture();
    let human = render_memory_file(
        &frontmatter("Person - Human", Some("person"), Some(&["Lo"])),
        "",
    )
    .expect("render");
    write_and_commit(&repo, "system/human.md", &human, "test: alias the human");

    let result = apply_facts_batch(
        &repo,
        batch(
            "77777777-7777-4777-8777-777777777777",
            vec![person_record(
                "Lo",
                &[],
                "Lo prefers dark themes.",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    assert_eq!(result.outcome(), "committed");
    assert_eq!(
        result.affected_paths(),
        ["people/human/observations.md".to_string()]
    );
    assert_eq!(
        contents(&explicit_entries(&dir, "human")),
        vec!["Lo prefers dark themes."]
    );
    assert!(!dir.join("people/human/card.md").exists());
}

// --- extraction-person-lifecycle.test.ts

#[test]
fn given_existing_explicit_observation_when_equal_fact_arrives_then_n_increments_and_date_refreshes_without_duplicate()
 {
    let (_tmp, dir, repo) = fixture();
    commit_person_card(&repo, "mina", "Mina", &["Min"]);
    commit_observations(
        &repo,
        "mina",
        "Mina",
        "## Explicit\n\n- [2026-08-01] Mina prefers concise reviews.\n",
    );

    apply_facts_batch(
        &repo,
        batch(
            "88888888-8888-4888-8888-888888888888",
            vec![person_record(
                "Mina",
                &[],
                "mina   prefers concise  reviews",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    let entries = explicit_entries(&dir, "mina");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (
            entries[0].date.as_str(),
            entries[0].content.as_str(),
            entries[0].n
        ),
        ("2026-08-10", "Mina prefers concise reviews.", Some(2))
    );

    apply_facts_batch(
        &repo,
        batch(
            "99999999-9999-4999-8999-999999999999",
            vec![person_record(
                "Min",
                &[],
                "\u{ff2d}\u{ff49}\u{ff4e}\u{ff41} prefers concise reviews",
                "2026-08-11",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply nfkc");

    let reinforced = explicit_entries(&dir, "mina");
    assert_eq!(reinforced.len(), 1);
    assert_eq!(
        (
            reinforced[0].date.as_str(),
            reinforced[0].content.as_str(),
            reinforced[0].n
        ),
        ("2026-08-11", "Mina prefers concise reviews.", Some(3))
    );
}

#[test]
fn given_existing_explicit_observation_when_different_fact_arrives_then_it_appends_separate_line() {
    let (_tmp, dir, repo) = fixture();
    commit_person_card(&repo, "mina", "Mina", &["Min"]);
    commit_observations(
        &repo,
        "mina",
        "Mina",
        "## Explicit\n\n- [2026-08-01] Mina prefers concise reviews.\n",
    );

    apply_facts_batch(
        &repo,
        batch(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            vec![person_record(
                "Min",
                &[],
                "Mina prefers dark themes.",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    let entries = explicit_entries(&dir, "mina");
    assert_eq!(entries.len(), 2);
    assert_eq!(
        (entries[0].date.as_str(), entries[0].content.as_str()),
        ("2026-08-01", "Mina prefers concise reviews.")
    );
    assert_eq!(
        (entries[1].date.as_str(), entries[1].content.as_str()),
        ("2026-08-10", "Mina prefers dark themes.")
    );
}

#[test]
fn given_no_matching_card_when_named_person_fact_arrives_then_card_skeleton_and_ledger_are_created()
{
    let (_tmp, dir, repo) = fixture();

    let result = apply_facts_batch(
        &repo,
        batch(
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            vec![person_record(
                "Yeongyu",
                &["YG"],
                "Yeongyu reviews on Tuesdays.",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    assert_eq!(result.outcome(), "committed");
    assert_eq!(
        result.affected_paths(),
        [
            "people/yeongyu/card.md".to_string(),
            "people/yeongyu/observations.md".to_string()
        ]
    );
    let card = parse_memory_file(
        &std::fs::read_to_string(dir.join("people/yeongyu/card.md")).expect("card"),
    )
    .expect("parse");
    assert_eq!(card.frontmatter.description, "Person - Yeongyu");
    assert_eq!(card.frontmatter.kind.as_deref(), Some("person"));
    assert_eq!(card.frontmatter.aliases, Some(vec!["YG".to_string()]));
    assert_eq!(
        contents(&explicit_entries(&dir, "yeongyu")),
        vec!["Yeongyu reviews on Tuesdays."]
    );
}

#[test]
fn given_slug_taken_by_different_person_when_new_person_sanitizes_to_it_then_numeric_suffix_avoids_collision()
 {
    let (_tmp, dir, repo) = fixture();
    commit_person_card(&repo, "yeongyu", "Yeongyu Kim", &["Yoshi"]);

    apply_facts_batch(
        &repo,
        batch(
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            vec![person_record(
                "Yeongyu",
                &[],
                "Yeongyu joined the platform guild.",
                "2026-08-10",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    let card = parse_memory_file(
        &std::fs::read_to_string(dir.join("people/yeongyu-2/card.md")).expect("card"),
    )
    .expect("parse");
    assert_eq!(card.frontmatter.description, "Person - Yeongyu");
    assert_eq!(card.frontmatter.kind.as_deref(), Some("person"));
    assert_eq!(
        contents(&explicit_entries(&dir, "yeongyu-2")),
        vec!["Yeongyu joined the platform guild."]
    );
    assert!(!dir.join("people/yeongyu/observations.md").exists());
}

#[test]
fn given_two_facts_about_same_new_person_in_one_batch_when_applied_then_one_card_receives_both() {
    let (_tmp, dir, repo) = fixture();

    apply_facts_batch(
        &repo,
        batch(
            "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            vec![
                person_record(
                    "Yeongyu",
                    &["YG"],
                    "Yeongyu reviews on Tuesdays.",
                    "2026-08-10",
                ),
                person_record("YG", &[], "YG drafted the alias table.", "2026-08-10"),
            ],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    assert_eq!(people_dirs(&dir), vec!["yeongyu"]);
    assert_eq!(
        contents(&explicit_entries(&dir, "yeongyu")),
        vec![
            "Yeongyu reviews on Tuesdays.",
            "YG drafted the alias table."
        ]
    );
}

// --- extraction-routing-gate.test.ts

#[test]
fn given_people_routing_disabled_when_person_fact_arrives_then_it_falls_back_to_monthly_notes() {
    let (_tmp, dir, repo) = fixture();

    apply_facts_batch(
        &repo,
        batch(
            "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            vec![person_record(
                "Mina",
                &["Min"],
                "Mina prefers concise reviews.",
                "2026-08-10",
            )],
        ),
        &author(),
        ApplyFactsBatchOptions {
            people: Some(FactsPeopleRouting {
                enabled: false,
                ..PEOPLE
            }),
            ..Default::default()
        },
    )
    .expect("apply");

    assert!(monthly_notes_body(&dir).contains("- [2026-08-10] Mina prefers concise reviews."));
    assert!(!dir.join("people/mina").exists());
}

#[test]
fn given_unresolved_person_mention_when_applied_then_prefixed_bullet_is_stored_verbatim_in_monthly_notes()
 {
    let (_tmp, dir, repo) = fixture();

    apply_facts_batch(
        &repo,
        batch(
            "ffffffff-ffff-4fff-8fff-ffffffffffff",
            vec![project(
                "person-unresolved: A teammate said the launch slips.",
            )],
        ),
        &author(),
        with_people(),
    )
    .expect("apply");

    assert!(
        monthly_notes_body(&dir)
            .contains("- [2026-08-10] person-unresolved: A teammate said the launch slips.")
    );
    assert!(!dir.join("people").exists());
}
