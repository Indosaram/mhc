use super::*;

fn notice(affected: Vec<MemoryWriteAffectedFile>) -> MemoryWriteNotice {
    MemoryWriteNotice {
        sha: "0123456789abcdef".to_string(),
        subject: "save fact".to_string(),
        identity: "agent".to_string(),
        affected,
        size: None,
        timeline: MemoryWriteNoticeTimeline::default(),
    }
}

#[test]
fn ordinal_handles_the_teens_exception() {
    assert_eq!(ordinal(1), "1st");
    assert_eq!(ordinal(2), "2nd");
    assert_eq!(ordinal(3), "3rd");
    assert_eq!(ordinal(4), "4th");
    assert_eq!(ordinal(11), "11th");
    assert_eq!(ordinal(12), "12th");
    assert_eq!(ordinal(13), "13th");
    assert_eq!(ordinal(21), "21st");
    assert_eq!(ordinal(112), "112th");
}

#[test]
fn format_bytes_uses_one_decimal_below_ten_k() {
    assert_eq!(format_bytes(2_048), "2.0K");
    assert_eq!(format_bytes(0), "0.0K");
    assert_eq!(format_bytes(33_792), "33K");
    assert_eq!(format_bytes(10_240), "10K");
}

#[test]
fn short_sha_takes_seven_characters_and_drops_empty() {
    assert_eq!(short_sha("0123456789abcdef").as_deref(), Some("0123456"));
    assert_eq!(short_sha(""), None);
}

#[test]
fn a_committed_write_renders_the_accent_remembered_row() {
    let spec = memory_write_notice_spec(
        &notice(vec![MemoryWriteAffectedFile {
            path: "notes/fact.md".to_string(),
            insertions: 0,
            deletions: 0,
        }]),
        0.0,
        &MemoryNoticeArgs {
            command: Some("create".to_string()),
            file_path: Some("notes/fact.md".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(spec.glyph, "●");
    assert_eq!(spec.title, "Remembered");
    assert_eq!(spec.tone, "accent");
    assert_eq!(spec.why, "Updated 1 memory file (notes/fact.md).");
    assert_eq!(spec.detail.as_deref(), Some("0123456 · agent · save fact"));
}

#[test]
fn entries_today_appends_the_ordinal_to_the_title() {
    let mut value = notice(Vec::new());
    value.timeline.entries_today = Some(4);
    let spec = memory_write_notice_spec(&value, 0.0, &MemoryNoticeArgs::default());
    assert_eq!(spec.title, format!("Remembered{}4th entry today", " · "));
}

#[test]
fn a_pure_insertion_reads_as_growth() {
    let spec = memory_write_notice_spec(
        &notice(vec![MemoryWriteAffectedFile {
            path: "notes/fact.md".to_string(),
            insertions: 47,
            deletions: 0,
        }]),
        0.0,
        &MemoryNoticeArgs::default(),
    );
    assert_eq!(spec.why, "Added 47 lines to notes/fact.md.");
}

#[test]
fn a_delete_says_what_was_let_go() {
    let spec = memory_write_notice_spec(
        &notice(Vec::new()),
        0.0,
        &MemoryNoticeArgs {
            command: Some("delete".to_string()),
            file_path: Some("notes/old.md".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(spec.title, "Let go");
    assert_eq!(spec.why, "Cleared notes/old.md. One less thing to carry.");
}

#[test]
fn a_rename_says_where_it_moved() {
    let spec = memory_write_notice_spec(
        &notice(Vec::new()),
        0.0,
        &MemoryNoticeArgs {
            command: Some("rename".to_string()),
            old_path: Some("notes/a.md".to_string()),
            new_path: Some("notes/b.md".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(spec.why, "Moved notes/a.md to notes/b.md.");
}

#[test]
fn a_degraded_notice_keeps_the_title_and_drops_the_stat_lines() {
    let spec = memory_degraded_notice_spec(&MemoryNoticeArgs::default());
    assert_eq!(spec.title, "Remembered");
    assert_eq!(spec.why, "Saved a memory change.");
    assert!(spec.extra.is_empty());
    assert!(spec.detail.is_none());
}

#[test]
fn the_size_line_reads_system_total_and_files() {
    let mut value = notice(Vec::new());
    value.size = Some(MemoryWriteNoticeSize {
        system_bytes: 2_048,
        total_bytes: 33_792,
        file_count: 12,
    });
    let spec = memory_write_notice_spec(&value, 0.0, &MemoryNoticeArgs::default());
    assert_eq!(
        spec.extra[0].text,
        format!("system 2.0K injected{}33K total{}12 files", " · ", " · ")
    );
    assert_eq!(spec.extra[0].tone.as_deref(), Some("dim"));
}

#[test]
fn a_backlogged_timeline_turns_warning_toned() {
    let mut value = notice(Vec::new());
    value.timeline.unreflected_steps = Some(25);
    let spec = memory_write_notice_spec(&value, 0.0, &MemoryNoticeArgs::default());
    assert_eq!(spec.extra[0].text, "25 steps unreflected");
    assert_eq!(spec.extra[0].tone.as_deref(), Some("warning"));

    value.timeline.unreflected_steps = Some(1);
    let calm = memory_write_notice_spec(&value, 0.0, &MemoryNoticeArgs::default());
    assert_eq!(calm.extra[0].text, "1 step unreflected");
    assert_eq!(calm.extra[0].tone.as_deref(), Some("dim"));
}

#[test]
fn a_refused_write_is_dim_and_keeps_the_raw_message_expanded() {
    let spec = memory_failure_notice_spec(
        "memory: create: block already exists at notes/fact.md",
        &MemoryNoticeArgs::default(),
    );
    assert_eq!(spec.glyph, "○");
    assert_eq!(spec.title, "Not remembered");
    assert_eq!(spec.tone, "dim");
    assert_eq!(spec.why, "notes/fact.md already exists.");
    assert_eq!(
        spec.detail.as_deref(),
        Some("memory: create: block already exists at notes/fact.md")
    );
}

#[test]
fn a_refused_delete_says_could_not_let_go() {
    let spec = memory_failure_notice_spec(
        "memory: delete: 'file_path' must be a non-empty string",
        &MemoryNoticeArgs {
            command: Some("delete".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(spec.title, "Couldn't let go");
    assert_eq!(spec.why, "The request had no file path.");
}

#[test]
fn friendly_failure_rewrites_the_known_refusals() {
    assert_eq!(
        friendly_failure("memory: str_replace: old_string was not found in the target memory block"),
        "The text to replace was not in that memory."
    );
    assert_eq!(
        friendly_failure("memory: str_replace: notes/a.md is read_only and cannot be modified"),
        "notes/a.md is read-only."
    );
    assert_eq!(
        friendly_failure("memory: insert made no effective changes"),
        "Nothing needed to change."
    );
    assert_eq!(
        friendly_failure("memory: no memory identity bound to this session; enable omo memory"),
        "Memory is not ready for this session yet."
    );
    assert_eq!(friendly_failure(""), "Memory was left unchanged.");
}

#[test]
fn the_pending_line_names_the_command_target() {
    assert_eq!(
        memory_pending_line(&MemoryNoticeArgs {
            command: Some("create".to_string()),
            file_path: Some("notes/fact.md".to_string()),
            ..Default::default()
        }),
        format!("◌ Remembering{}notes/fact.md", " · ")
    );
    assert_eq!(
        memory_pending_line(&MemoryNoticeArgs {
            command: Some("delete".to_string()),
            file_path: Some("notes/old.md".to_string()),
            ..Default::default()
        }),
        format!("◌ Letting go{}notes/old.md", " · ")
    );
    assert_eq!(
        memory_pending_line(&MemoryNoticeArgs {
            command: Some("rename".to_string()),
            old_path: Some("a.md".to_string()),
            new_path: Some("b.md".to_string()),
            ..Default::default()
        }),
        format!("◌ Moving{}a.md → b.md", " · ")
    );
}

#[test]
fn parse_numstat_reads_plain_records_and_renames() {
    let plain = "3\t1\tnotes/fact.md\0";
    assert_eq!(
        parse_numstat(plain),
        vec![MemoryWriteAffectedFile {
            path: "notes/fact.md".to_string(),
            insertions: 3,
            deletions: 1,
        }]
    );

    let rename = "2\t0\t\0notes/old.md\0notes/new.md\0";
    assert_eq!(
        parse_numstat(rename),
        vec![MemoryWriteAffectedFile {
            path: "notes/new.md".to_string(),
            insertions: 2,
            deletions: 0,
        }]
    );
}

#[test]
fn parse_numstat_reports_binary_counts_as_zero_and_skips_empty_fields() {
    let binary = "-\t-\tnotes/image.md\0";
    assert_eq!(
        parse_numstat(binary),
        vec![MemoryWriteAffectedFile {
            path: "notes/image.md".to_string(),
            insertions: 0,
            deletions: 0,
        }]
    );
    assert!(parse_numstat("\0").is_empty());
}

#[test]
fn gather_write_notice_reads_the_committed_affected_files() {
    use memory_core::git::{GitCommitAuthor, GitMemoryRepo, GitSeedFile, InitializeGitRepoOptions};
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "notice-agent").expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![GitSeedFile {
            relative_path: "notes/fact.md".to_string(),
            content: "---\ndescription: Fact\n---\nbody\n".to_string(),
        }],
        ..Default::default()
    }))
    .expect("init");
    let author = GitCommitAuthor {
        agent_id: "notice-agent".to_string(),
        author_name: "Notice Agent".to_string(),
        author_email: None,
    };
    std::fs::write(
        repo.dir.join("notes/fact.md"),
        "---\ndescription: Fact\n---\nbody\nmore\n",
    )
    .expect("edit");
    let commit = repo
        .commit_write(&["notes/fact.md"], "save fact", &author)
        .expect("commit");

    let notice = gather_write_notice(&repo, &commit.sha, "save fact", "notice-agent");
    assert_eq!(notice.sha, commit.sha);
    assert_eq!(notice.subject, "save fact");
    assert_eq!(notice.identity, "notice-agent");
    assert_eq!(notice.affected.len(), 1);
    assert_eq!(notice.affected[0].path, "notes/fact.md");
    assert_eq!(notice.affected[0].insertions, 1);
    assert!(notice.size.is_none());
    assert!(notice.timeline.entries_today.is_none());
}

#[test]
fn remembered_title_joins_the_detail() {
    assert_eq!(remembered_title(Some("4th entry today")), format!("● Remembered{}4th entry today", " · "));
    assert_eq!(remembered_title(None), "● Remembered");
}
