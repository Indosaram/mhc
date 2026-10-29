//! Atomic revision publication and view materialization.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::domain_types::{UlwLoopLedgerEntry, UlwLoopPlan};
use crate::ledger::read_ledger_from_records;
use crate::paths::{ulw_loop_dir, ulw_loop_state_lock_path, UlwLoopScope};
use crate::plan_log::{assert_projection_complete, read_optional, read_records, reconcile_plan, PlanCommitRecord};
use crate::runtime::UlwLoopError;

pub struct CommitMutation<'a> {
    pub plan: &'a mut UlwLoopPlan,
    pub entries: &'a mut [UlwLoopLedgerEntry],
}

pub fn commit(
    repo_root: impl AsRef<Path>,
    scope: Option<&UlwLoopScope>,
    mutation: CommitMutation,
) -> Result<(), UlwLoopError> {
    let dir = ulw_loop_dir(&repo_root, scope);
    let next = mutation.plan.revision.unwrap_or(0) + 1;
    let brief = mutation
        .plan
        .brief
        .clone()
        .or_else(|| read_optional(dir.join("brief.md")))
        .unwrap_or_default();

    let mut ledger_entries = Vec::new();
    for (seq, entry) in mutation.entries.iter_mut().enumerate() {
        entry.revision = Some(next);
        entry.id = Some(format!("{next}-{seq}"));
        ledger_entries.push(entry.clone());
    }

    let record = PlanCommitRecord {
        version: 1,
        revision: next,
        plan: UlwLoopPlan {
            revision: Some(next),
            brief: Some(brief.clone()),
            ..mutation.plan.clone()
        },
        ledger: ledger_entries,
    };

    let revisions_dir = dir.join("revisions");
    let tmp_dir = dir.join("tmp");
    let _ = fs::create_dir_all(&revisions_dir);
    let _ = fs::create_dir_all(&tmp_dir);

    let temp_name = format!("{}-{}.json", std::process::id(), uuid::Uuid::new_v4());
    let temp_path = tmp_dir.join(&temp_name);
    let target_name = format!("{:08}.json", next);
    let target_path = revisions_dir.join(&target_name);

    let content = serde_json::to_string(&record).map_err(|e| {
        UlwLoopError::new(e.to_string(), "ULW_LOOP_SERIALIZE_ERROR")
    })?;

    {
        let mut file = File::create(&temp_path).map_err(|e| {
            UlwLoopError::new(e.to_string(), "ULW_LOOP_IO_ERROR")
        })?;
        file.write_all(format!("{content}\n").as_bytes()).map_err(|e| {
            UlwLoopError::new(e.to_string(), "ULW_LOOP_IO_ERROR")
        })?;
        file.flush().map_err(|e| {
            UlwLoopError::new(e.to_string(), "ULW_LOOP_IO_ERROR")
        })?;
    }

    let link_res = fs::hard_link(&temp_path, &target_path);
    let _ = fs::remove_file(&temp_path);

    if let Err(err) = link_res {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(UlwLoopError::new(
                "Another writer published the read revision.",
                "ULW_LOOP_PUBLISH_CONFLICT",
            ));
        }
        return Err(UlwLoopError::new(
            "Immutable ulw-loop publication requires atomic hard links.",
            "ULW_LOOP_PUBLISH_UNSUPPORTED_FS",
        ));
    }

    mutation.plan.revision = Some(next);
    mutation.plan.brief = Some(brief);

    materialize_sync(&dir)?;
    Ok(())
}

fn view_contents(dir: &Path) -> Result<Vec<(String, String)>, UlwLoopError> {
    let plan = match reconcile_plan(dir) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let records = read_records(dir);
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let reset = plan.ledger_reset_revision.unwrap_or(0);
    let ledger = read_ledger_from_records(dir, records, reset);
    assert_projection_complete(&plan, &ledger)?;

    let goals_content = format!(
        "{}\n",
        serde_json::to_string_pretty(&plan).map_err(|e| {
            UlwLoopError::new(e.to_string(), "ULW_LOOP_SERIALIZE_ERROR")
        })?
    );

    let ledger_content = if ledger.is_empty() {
        String::new()
    } else {
        let lines: Vec<_> = ledger
            .iter()
            .map(|e| serde_json::to_string(e).unwrap_or_default())
            .collect();
        format!("{}\n", lines.join("\n"))
    };

    let brief_content = plan
        .brief
        .clone()
        .or_else(|| read_optional(dir.join("brief.md")))
        .unwrap_or_default();

    let views = vec![
        ("goals.json".to_string(), goals_content),
        ("ledger.jsonl".to_string(), ledger_content),
        ("brief.md".to_string(), brief_content),
    ];

    let mut filtered = Vec::new();
    for (name, content) in views {
        let current = read_optional(dir.join(&name));
        if current.as_deref() != Some(&content) {
            filtered.push((name, content));
        }
    }
    Ok(filtered)
}

pub fn materialize_sync(dir: impl AsRef<Path>) -> Result<(), UlwLoopError> {
    let dir = dir.as_ref();
    let contents = view_contents(dir)?;
    if contents.is_empty() {
        return Ok(());
    }
    let tmp_dir = dir.join("tmp");
    let _ = fs::create_dir_all(&tmp_dir);

    for (name, content) in contents {
        let temp_name = format!("{}-{}-{}", std::process::id(), uuid::Uuid::new_v4(), name);
        let temp_path = tmp_dir.join(&temp_name);
        {
            let mut file = File::create(&temp_path).map_err(|e| {
                UlwLoopError::new(e.to_string(), "ULW_LOOP_IO_ERROR")
            })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                UlwLoopError::new(e.to_string(), "ULW_LOOP_IO_ERROR")
            })?;
            file.flush().map_err(|e| {
                UlwLoopError::new(e.to_string(), "ULW_LOOP_IO_ERROR")
            })?;
        }
        let _ = fs::rename(&temp_path, dir.join(&name));
        let _ = fs::remove_file(&temp_path);
    }
    Ok(())
}

pub fn materialize(dir: impl AsRef<Path>) -> Result<(), UlwLoopError> {
    materialize_sync(dir)
}
