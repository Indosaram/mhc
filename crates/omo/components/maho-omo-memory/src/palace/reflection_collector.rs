use std::path::Path;

use memory_core::identity::MemoryIdentityPaths;
use memory_core::journal::{ReflectionTranscriptState, parse_state};
use serde::Serialize;

use super::PalaceError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceReflectionOutcome {
    pub run_id: String,
    pub outcome: String,
    pub finished_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PalaceReflection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ReflectionTranscriptState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub outcomes: Vec<PalaceReflectionOutcome>,
}

pub fn collect_reflection(
    paths: &MemoryIdentityPaths,
    limit: usize,
) -> Result<PalaceReflection, PalaceError> {
    let cursor = read_latest_cursor(&paths.transcripts);
    let outcomes = read_outcomes(&paths.reflection.join("completions"), limit);
    Ok(match cursor {
        Some((conversation_id, state)) => PalaceReflection {
            cursor: Some(state),
            conversation_id: Some(conversation_id),
            outcomes,
        },
        None => PalaceReflection {
            cursor: None,
            conversation_id: None,
            outcomes,
        },
    })
}

fn read_latest_cursor(transcripts_dir: &Path) -> Option<(String, ReflectionTranscriptState)> {
    let conversations = std::fs::read_dir(transcripts_dir).ok()?;
    let mut latest: Option<(String, ReflectionTranscriptState, std::time::SystemTime)> = None;
    for entry in conversations.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let state_path = entry.path().join("state.json");
        let Some(state) = read_state(&state_path) else {
            continue;
        };
        let modified_at = std::fs::metadata(&state_path)
            .and_then(|info| info.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let conversation_id = entry.file_name().to_string_lossy().to_string();
        let newer = latest
            .as_ref()
            .map(|(_, _, current)| modified_at > *current)
            .unwrap_or(true);
        if newer {
            latest = Some((conversation_id, state, modified_at));
        }
    }
    latest.map(|(conversation_id, state, _)| (conversation_id, state))
}

fn read_state(path: &Path) -> Option<ReflectionTranscriptState> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_state(&raw).ok()
}

fn read_outcomes(completions_dir: &Path, limit: usize) -> Vec<PalaceReflectionOutcome> {
    let Ok(files) = std::fs::read_dir(completions_dir) else {
        return Vec::new();
    };
    let mut outcomes = Vec::new();
    for file in files.flatten() {
        let name = file.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Some(parsed) = read_json(&file.path()) else {
            continue;
        };
        if !parsed.is_object() {
            continue;
        }
        let run_id = parsed
            .get("runId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| stem.to_string());
        let outcome = parsed
            .get("outcome")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let finished_at = parsed
            .get("finishedAt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        outcomes.push(PalaceReflectionOutcome {
            run_id,
            outcome,
            finished_at,
        });
    }
    outcomes.sort_by(|left, right| right.finished_at.cmp(&left.finished_at));
    outcomes.truncate(limit);
    outcomes
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::test_support::create_palace_fixture;
    use serde_json::json;

    #[test]
    fn cursor_and_recent_outcomes_are_newest_first() {
        let fixture = create_palace_fixture(false);
        fixture.write_completion(
            "run-old",
            json!({"runId": "run-old", "outcome": "merged", "finishedAt": "2026-01-01T00:00:00.000Z"}),
        );
        fixture.write_completion(
            "run-new",
            json!({"runId": "run-new", "outcome": "failed", "finishedAt": "2026-02-01T00:00:00.000Z"}),
        );

        let reflection = collect_reflection(&fixture.paths, 5).unwrap();

        let cursor = reflection.cursor.unwrap();
        assert_eq!(cursor.total_completed_steps, 2);
        assert_eq!(cursor.reflected_completed_steps, 1);
        assert_eq!(reflection.conversation_id.as_deref(), Some("conversation-1"));
        assert_eq!(
            reflection.outcomes.iter().map(|entry| entry.run_id.as_str()).collect::<Vec<_>>(),
            ["run-new", "run-old"]
        );
        assert_eq!(reflection.outcomes[0].outcome, "failed");
    }

    #[test]
    fn only_newest_limit_is_kept() {
        let fixture = create_palace_fixture(false);
        for index in 0..5 {
            fixture.write_completion(
                &format!("run-{index}"),
                json!({
                    "runId": format!("run-{index}"),
                    "outcome": "merged",
                    "finishedAt": format!("2026-03-0{}T00:00:00.000Z", index + 1),
                }),
            );
        }

        let reflection = collect_reflection(&fixture.paths, 2).unwrap();

        assert_eq!(
            reflection.outcomes.iter().map(|entry| entry.run_id.as_str()).collect::<Vec<_>>(),
            ["run-4", "run-3"]
        );
    }
}
