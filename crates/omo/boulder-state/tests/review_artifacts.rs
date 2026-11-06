//! Translation of `storage/review-artifacts.test.ts`.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use boulder_state::get_plan_reviews_dir;
use boulder_state::{
    MomusReviewArtifactInput, MomusReviewVerdict, format_momus_review_artifact, hash_plan_content,
    write_momus_review_artifact,
};
use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;

fn create_temp_project() -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix("boulder-review-artifacts-")
        .tempdir()
        .expect("temp project");
    fs::create_dir_all(directory.path().join(".omo").join("plans")).expect("plans dir");
    fs::write(
        directory.path().join(".omo").join("plans").join("demo.md"),
        "# Demo plan\n",
    )
    .expect("plan");
    directory
}

fn at(text: &str) -> DateTime<Utc> {
    text.parse().expect("timestamp")
}

fn artifact(verdict: MomusReviewVerdict) -> MomusReviewArtifactInput {
    MomusReviewArtifactInput {
        verdict,
        reviewed_at: at("2026-04-01T00:00:00.000Z"),
        model: None,
        plan_hash: None,
        duration_ms: None,
        session_id: None,
        reviewer_output: "**[OKAY]**".to_string(),
    }
}

fn write(directory: &Path, plan_path: &str, input: &MomusReviewArtifactInput) -> Option<PathBuf> {
    write_momus_review_artifact(directory, plan_path, input).expect("write artifact")
}

fn ends_with(path: &Option<PathBuf>, tail: &Path) -> bool {
    path.as_ref().is_some_and(|path| path.ends_with(tail))
}

fn sorted_entries(directory: &Path) -> Vec<String> {
    let mut entries: Vec<String> = fs::read_dir(directory)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    entries
}

fn reviews_dir(directory: &Path) -> PathBuf {
    directory.join(".omo").join("plans").join("demo.reviews")
}

#[cfg(unix)]
#[test]
fn get_plan_reviews_dir_resolves_a_sibling_reviews_directory_for_a_relative_plan_path() {
    let directory = Path::new("/project");
    assert_eq!(
        get_plan_reviews_dir(directory, ".omo/plans/demo.md"),
        Path::new("/project/.omo/plans/demo.reviews")
    );
}

#[cfg(unix)]
#[test]
fn get_plan_reviews_dir_keeps_absolute_plan_paths_rooted_at_their_own_location() {
    assert_eq!(
        get_plan_reviews_dir(Path::new("/project"), "/elsewhere/.omo/plans/demo.md"),
        Path::new("/elsewhere/.omo/plans/demo.reviews")
    );
}

#[test]
fn hash_plan_content_produces_a_stable_sha256_hex_digest() {
    let first = hash_plan_content("# Demo plan\n");
    let second = hash_plan_content("# Demo plan\n");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(
        first
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    );
}

#[test]
fn hash_plan_content_changes_when_the_plan_content_changes() {
    let before = hash_plan_content("# Demo plan\n");
    let after = hash_plan_content("# Demo plan\n- [ ] new todo\n");
    assert_ne!(after, before);
}

#[test]
fn writes_001_reject_md_for_the_first_review_of_a_plan() {
    let directory = create_temp_project();
    let input = MomusReviewArtifactInput {
        model: Some("anthropic/claude-opus-4-6".to_string()),
        plan_hash: Some(hash_plan_content("# Demo plan\n")),
        duration_ms: Some(95_000.0),
        reviewer_output: "**[REJECT]**\n\n**Summary**: Reference on line 12 does not exist."
            .to_string(),
        ..artifact(MomusReviewVerdict::Reject)
    };

    let written = write(directory.path(), ".omo/plans/demo.md", &input);

    assert!(ends_with(
        &written,
        &Path::new("demo.reviews").join("001-reject.md")
    ));
    let content = fs::read_to_string(written.expect("written")).expect("content");
    assert!(content.contains("Verdict: reject"));
    assert!(content.contains("Reviewed at: 2026-04-01T00:00:00.000Z"));
    assert!(content.contains("Model: anthropic/claude-opus-4-6"));
    assert!(content.contains("Duration: 95.0s"));
    assert!(content.contains("**[REJECT]**"));
}

#[test]
fn increments_the_sequence_across_reviews_of_the_same_plan() {
    let directory = create_temp_project();

    let first = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Reject),
    );
    let second = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert!(ends_with(&first, Path::new("001-reject.md")));
    assert!(ends_with(&second, Path::new("002-okay.md")));
    assert_eq!(
        sorted_entries(&reviews_dir(directory.path())),
        vec!["001-reject.md", "002-okay.md"]
    );
}

#[test]
fn ignores_unrelated_files_when_picking_the_next_sequence() {
    let directory = create_temp_project();
    let reviews = reviews_dir(directory.path());
    fs::create_dir_all(&reviews).expect("reviews dir");
    fs::write(reviews.join("notes.txt"), "not a review").expect("notes");
    fs::write(reviews.join("007-okay.md"), "older review").expect("older");

    let written = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert!(ends_with(&written, Path::new("008-okay.md")));
    assert!(reviews.join("008-okay.md").exists());
}

#[test]
fn refuses_a_plan_path_that_escapes_the_plans_directory_with_dot_dot_segments() {
    let directory = create_temp_project();

    let written = write(
        directory.path(),
        ".omo/plans/../../escaped.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(written, None);
    assert!(!directory.path().join("escaped.reviews").exists());
    assert!(!reviews_dir(directory.path()).exists());
}

#[test]
fn refuses_an_absolute_plan_path_that_belongs_to_another_project() {
    let directory = create_temp_project();
    let other_project = create_temp_project();
    let other_plan = other_project
        .path()
        .join(".omo")
        .join("plans")
        .join("demo.md");

    let written = write(
        directory.path(),
        &other_plan.to_string_lossy(),
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(written, None);
    assert!(!reviews_dir(other_project.path()).exists());
}

#[cfg(unix)]
#[test]
fn refuses_a_plan_file_that_is_a_symlink_to_a_file_outside_the_project() {
    let directory = create_temp_project();
    let outside = tempfile::tempdir().expect("outside");
    let outside_plan = outside.path().join("outside.md");
    fs::write(&outside_plan, "# Outside plan\n").expect("outside plan");
    std::os::unix::fs::symlink(
        &outside_plan,
        directory
            .path()
            .join(".omo")
            .join("plans")
            .join("linked.md"),
    )
    .expect("symlink");

    let written = write(
        directory.path(),
        ".omo/plans/linked.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(written, None);
    assert!(!outside.path().join("outside.reviews").exists());
}

#[cfg(unix)]
#[test]
fn refuses_to_write_through_a_symlinked_reviews_directory() {
    let directory = create_temp_project();
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), reviews_dir(directory.path())).expect("symlink");

    let written = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(written, None);
    assert_eq!(sorted_entries(outside.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn never_writes_through_a_symlinked_artifact_file() {
    let directory = create_temp_project();
    let reviews = reviews_dir(directory.path());
    fs::create_dir_all(&reviews).expect("reviews dir");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("target.md");
    fs::write(&outside_file, "sentinel").expect("sentinel");
    std::os::unix::fs::symlink(&outside_file, reviews.join("001-okay.md")).expect("symlink");

    let written = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert!(ends_with(&written, Path::new("002-okay.md")));
    assert_eq!(
        fs::read_to_string(&outside_file).expect("sentinel"),
        "sentinel"
    );
    assert!(
        fs::read_to_string(reviews.join("002-okay.md"))
            .expect("artifact")
            .contains("Verdict: okay")
    );
}

#[cfg(unix)]
#[test]
fn refuses_to_write_when_omo_is_a_symlink_to_a_directory_outside_the_project() {
    let directory = tempfile::tempdir().expect("project");
    let outside = tempfile::tempdir().expect("outside");
    fs::create_dir_all(outside.path().join("plans")).expect("plans");
    fs::write(
        outside.path().join("plans").join("demo.md"),
        "# Demo plan\n",
    )
    .expect("plan");
    std::os::unix::fs::symlink(outside.path(), directory.path().join(".omo")).expect("symlink");

    let written = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(written, None);
    assert!(!outside.path().join("plans").join("demo.reviews").exists());
    assert_eq!(
        sorted_entries(&outside.path().join("plans")),
        vec!["demo.md"]
    );
}

#[cfg(unix)]
#[test]
fn refuses_to_write_when_omo_plans_is_a_symlink_to_a_directory_outside_the_project() {
    let directory = tempfile::tempdir().expect("project");
    fs::create_dir_all(directory.path().join(".omo")).expect(".omo");
    let outside = tempfile::tempdir().expect("outside");
    let outside_plans = outside.path().join("plans");
    fs::create_dir_all(&outside_plans).expect("plans");
    fs::write(outside_plans.join("demo.md"), "# Demo plan\n").expect("plan");
    std::os::unix::fs::symlink(&outside_plans, directory.path().join(".omo").join("plans"))
        .expect("symlink");

    let written = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(written, None);
    assert!(!outside_plans.join("demo.reviews").exists());
    assert_eq!(sorted_entries(&outside_plans), vec!["demo.md"]);
}

#[test]
fn writes_a_gitignore_next_to_the_plans_that_covers_the_reviews_directories() {
    let directory = create_temp_project();

    let written = write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert!(written.is_some());
    let gitignore = fs::read_to_string(
        directory
            .path()
            .join(".omo")
            .join("plans")
            .join(".gitignore"),
    )
    .expect("gitignore");
    assert!(gitignore.contains("*.reviews"));
}

#[test]
fn keeps_an_existing_plans_gitignore_untouched() {
    let directory = create_temp_project();
    let gitignore_path = directory
        .path()
        .join(".omo")
        .join("plans")
        .join(".gitignore");
    fs::write(&gitignore_path, "custom-rule\n").expect("gitignore");

    write(
        directory.path(),
        ".omo/plans/demo.md",
        &artifact(MomusReviewVerdict::Okay),
    );

    assert_eq!(
        fs::read_to_string(&gitignore_path).expect("gitignore"),
        "custom-rule\n"
    );
}

#[test]
fn returns_the_existing_artifact_when_the_same_reviewer_session_is_persisted_twice() {
    let directory = create_temp_project();
    let base = MomusReviewArtifactInput {
        session_id: Some("ses_review_1".to_string()),
        ..artifact(MomusReviewVerdict::Okay)
    };

    let first = write(directory.path(), ".omo/plans/demo.md", &base);
    let second = write(
        directory.path(),
        ".omo/plans/demo.md",
        &MomusReviewArtifactInput {
            reviewed_at: at("2026-04-01T01:00:00.000Z"),
            ..base.clone()
        },
    );
    let third = write(
        directory.path(),
        ".omo/plans/demo.md",
        &MomusReviewArtifactInput {
            reviewed_at: at("2026-04-01T02:00:00.000Z"),
            session_id: Some("ses_review_2".to_string()),
            ..base.clone()
        },
    );

    assert!(ends_with(&first, Path::new("001-okay.md")));
    assert_eq!(second, first);
    assert!(ends_with(&third, Path::new("002-okay.md")));
    assert_eq!(
        sorted_entries(&reviews_dir(directory.path())),
        vec!["001-okay.md", "002-okay.md"]
    );
}

#[test]
fn does_not_treat_a_session_id_that_prefixes_an_earlier_one_as_a_duplicate() {
    let directory = create_temp_project();
    let base = artifact(MomusReviewVerdict::Okay);

    let first = write(
        directory.path(),
        ".omo/plans/demo.md",
        &MomusReviewArtifactInput {
            session_id: Some("ses_review_1".to_string()),
            ..base.clone()
        },
    );
    let second = write(
        directory.path(),
        ".omo/plans/demo.md",
        &MomusReviewArtifactInput {
            session_id: Some("ses_review".to_string()),
            ..base
        },
    );

    assert!(ends_with(&first, Path::new("001-okay.md")));
    assert!(ends_with(&second, Path::new("002-okay.md")));
    assert_eq!(
        sorted_entries(&reviews_dir(directory.path())),
        vec!["001-okay.md", "002-okay.md"]
    );
}

#[test]
fn format_marks_missing_model_hash_and_duration_as_unknown_or_unavailable() {
    let content =
        format_momus_review_artifact(3, ".omo/plans/demo.md", &artifact(MomusReviewVerdict::Okay));

    assert!(content.contains("# Momus review 003: OKAY"));
    assert!(content.contains("Model: unknown"));
    assert!(content.contains("Plan hash (sha256): unavailable"));
    assert!(content.contains("Duration: unknown"));
}
