//! Stop resume hook for recovering turn-death in ulw-loop.

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain_types::{UlwLoopItem, UlwLoopPlan};
use crate::ledger::read_ledger_at;
use crate::paths::{normalize_ulw_loop_session_id, ulw_loop_dir, ulw_loop_state_lock_path, UlwLoopScope};
use crate::plan_io::read_ulw_loop_plan;
use crate::state_lock::with_state_lock;

const RESUME_CAP: u32 = 2;

const CONTEXT_PRESSURE_MARKERS: [&str; 7] = [
    "context compacted",
    "context_length_exceeded",
    "skill descriptions were shortened",
    "context_too_large",
    "codex ran out of room in the model's context window",
    "your input exceeds the context window",
    "long threads and multiple compactions",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopPayload {
    pub session_id: String,
    pub cwd: String,
    pub transcript_path: String,
    pub stop_hook_active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopHookOutput {
    pub decision: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Counter {
    count: u32,
    #[serde(rename = "ledgerLineCount")]
    ledger_line_count: usize,
}

pub fn run_stop_resume_hook(input: &Value) -> String {
    let Some(payload) = parse_stop_payload(input) else {
        return String::new();
    };
    if payload.stop_hook_active {
        return String::new();
    }
    if transcript_shows_context_pressure(&payload.transcript_path) {
        return String::new();
    }
    if boulder_continuation_will_fire(&payload.cwd, &payload.session_id) {
        return String::new();
    }

    let scope = UlwLoopScope {
        session_id: Some(payload.session_id.clone()),
        ..Default::default()
    };
    let (state_dir, plan) = match read_ulw_loop_plan(&payload.cwd, Some(&scope)) {
        Ok(plan) => (ulw_loop_dir(&payload.cwd, Some(&scope)), plan),
        Err(_) => {
            let Ok(plan) = read_ulw_loop_plan(&payload.cwd, None) else {
                return String::new();
            };
            (ulw_loop_dir(&payload.cwd, None), plan)
        }
    };

    if plan.aggregate_completion.as_ref().is_some_and(|a| a.status == "complete") {
        return String::new();
    }

    let Some(goal) = resumable_goal(&plan) else {
        return String::new();
    };

    let lock_path = ulw_loop_state_lock_path(&payload.cwd, Some(&scope));
    if !consume_resume_budget_locked(&lock_path, &state_dir, &goal.id) {
        return String::new();
    }

    let output = StopHookOutput {
        decision: "block".to_string(),
        reason: render_resume_directive(&plan, goal, &payload.session_id),
    };

    serde_json::to_string(&output).unwrap_or_default()
}

fn resumable_goal(plan: &UlwLoopPlan) -> Option<&UlwLoopItem> {
    if let Some(active_id) = &plan.active_goal_id {
        if let Some(active) = plan.goals.iter().find(|g| &g.id == active_id) {
            if is_resumable_status(&active.status) {
                return Some(active);
            }
        }
    }
    plan.goals.iter().find(|g| is_resumable_status(&g.status))
}

fn is_resumable_status(status: &str) -> bool {
    status == "pending" || status == "in_progress"
}

fn consume_resume_budget_locked(lock_path: &Path, state_dir: &Path, goal_id: &str) -> bool {
    with_state_lock(lock_path, None, |_| {
        Ok(consume_resume_budget(state_dir, goal_id))
    }).unwrap_or(false)
}

fn consume_resume_budget(state_dir: &Path, goal_id: &str) -> bool {
    let ledger_line_count = read_ledger_at(state_dir).len();
    let counter_path = state_dir.join(format!("auto-resume-{goal_id}.json"));
    let stuck_path = state_dir.join(format!("auto-resume-{goal_id}.stuck"));

    if !is_inside_dir(state_dir, &counter_path) || !is_inside_dir(state_dir, &stuck_path) {
        return false;
    }

    let previous = read_counter(&counter_path);
    let count = match previous {
        Some(p) if p.ledger_line_count == ledger_line_count => p.count,
        _ => 0,
    };

    if count >= RESUME_CAP {
        let _ = fs::write(&stuck_path, format!("no ledger progress after {count} resumes\n"));
        return false;
    }

    let counter = Counter {
        count: count + 1,
        ledger_line_count,
    };
    if let Ok(json) = serde_json::to_string(&counter) {
        let _ = fs::write(&counter_path, json);
    }
    true
}

fn is_inside_dir(dir: &Path, candidate: &Path) -> bool {
    let Ok(canon_dir) = dir.canonicalize() else {
        return false;
    };
    let Some(parent) = candidate.parent() else {
        return false;
    };
    let Ok(canon_parent) = parent.canonicalize() else {
        return false;
    };
    canon_parent.starts_with(&canon_dir)
}

fn render_resume_directive(plan: &UlwLoopPlan, goal: &UlwLoopItem, session_id: &str) -> String {
    let normalized = normalize_ulw_loop_session_id(Some(session_id));
    let option = match &normalized {
        Some(norm) if plan.goals_path.contains(&format!("/{norm}/")) => format!(" --session-id {norm}"),
        _ => String::new(),
    };

    vec![
        format!("The ulw-loop run in this session still has unfinished goals (next: {} — {}).", goal.id, goal.title),
        "The turn ended before the loop completed. Resume it now:".to_string(),
        format!("1. Run `mhc ulw-loop status{option} --json` to reload the plan, the active goal, and currentAttemptDir."),
        "2. Continue the active goal's remaining success criteria, recording evidence with record-evidence.".to_string(),
        format!("3. Checkpoint through `mhc ulw-loop checkpoint{option}` when the goal's criteria are proven; a complete checkpoint prints the next goal instruction."),
        "If the loop is genuinely blocked on the user, checkpoint the goal as blocked with the reason instead.".to_string(),
    ].join("\n")
}

fn read_counter(counter_path: &Path) -> Option<Counter> {
    let content = fs::read_to_string(counter_path).ok()?;
    serde_json::from_str(&content).ok()
}

fn boulder_continuation_will_fire(cwd: &str, session_id: &str) -> bool {
    let boulder_path = Path::new(cwd).join(".omo").join("boulder.json");
    let Ok(content) = fs::read_to_string(&boulder_path) else {
        return false;
    };
    let Ok(raw) = serde_json::from_str::<Value>(&content) else {
        return false;
    };

    let entries: Vec<&Value> = if let Some(works) = raw.get("works").and_then(|v| v.as_object()) {
        works.values().collect()
    } else {
        vec![&raw]
    };

    entries.iter().any(|work| {
        let Some(obj) = work.as_object() else { return false; };
        let session_ids = obj.get("session_ids").and_then(|v| v.as_array());
        let status = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let continuable = status == "active" || status == "paused";
        let target_sid = format!("codex:{session_id}");
        let has_session = session_ids.is_some_and(|arr| arr.iter().any(|s| s.as_str() == Some(&target_sid)));
        continuable && has_session && boulder_plan_has_checklist(cwd, obj)
    })
}

fn transcript_shows_context_pressure(transcript_path: &str) -> bool {
    use std::io::BufRead;
    let Ok(file) = fs::File::open(transcript_path) else {
        return false;
    };
    // Pin `read_to_string` semantics: any read or UTF-8 error makes the whole transcript `false`,
    // even after an earlier line matched; markers never span a newline.
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut found = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return found,
            Ok(_) => {
                if !found {
                    let lower = line.to_lowercase();
                    found = CONTEXT_PRESSURE_MARKERS.iter().any(|m| lower.contains(m));
                }
            }
            Err(_) => return false,
        }
    }
}

fn boulder_plan_has_checklist(cwd: &str, entry: &serde_json::Map<String, Value>) -> bool {
    let Some(active_plan) = entry.get("active_plan").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) else {
        return false;
    };
    let plan_path = if Path::new(active_plan).is_absolute() {
        PathBuf::from(active_plan)
    } else {
        Path::new(cwd).join(active_plan)
    };

    let worktree = entry.get("worktree_path").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    let candidates = if let (Some(wt), false) = (worktree, Path::new(active_plan).is_absolute()) {
        let wt_path = if Path::new(wt).is_absolute() {
            PathBuf::from(wt)
        } else {
            Path::new(cwd).join(wt)
        };
        vec![wt_path.join(active_plan), plan_path]
    } else {
        vec![plan_path]
    };

    for cand in candidates {
        if let Ok(content) = fs::read_to_string(&cand) {
            if content.lines().any(|l| l.starts_with("- [ ] ") || l.starts_with("- [x] ") || l.starts_with("- [X] ")) {
                return true;
            }
        }
    }
    false
}

fn parse_stop_payload(value: &Value) -> Option<StopPayload> {
    let obj = value.as_object()?;
    if obj.get("hook_event_name").and_then(|v| v.as_str()) != Some("Stop") {
        return None;
    }
    let session_id = obj.get("session_id").and_then(|v| v.as_str())?.to_string();
    let cwd = obj.get("cwd").and_then(|v| v.as_str())?.to_string();
    let transcript_path = obj.get("transcript_path").and_then(|v| v.as_str())?.to_string();
    let stop_hook_active = obj.get("stop_hook_active").and_then(|v| v.as_bool())?;

    Some(StopPayload {
        session_id,
        cwd,
        transcript_path,
        stop_hook_active,
    })
}
