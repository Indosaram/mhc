//! `thread_archive` operation (`thread/tools/archive-ops.ts`).

use serde_json::{Value, json};

use super::super::contracts::params::ThreadArchiveInput;
use super::super::control::locate::{LocatedControl, locate_control};
use super::super::control::port::ControlError;
use super::internals::failure;
use super::ports::{ThreadHostView, ThreadToolSurfaceOptions};

pub async fn archive_thread(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    value: &ThreadArchiveInput,
    caller_id: &str,
) -> Result<Value, ControlError> {
    let archived = value.archived.unwrap_or(true);
    let located = locate_control(
        options,
        current,
        &value.thread,
        value.all_scope.unwrap_or(false),
        caller_id,
    )
    .await;
    let control = match located {
        LocatedControl::Error(err) => return Ok(err),
        LocatedControl::NotLive { thread_id, .. } => {
            return Ok(failure(
                "not_resumable",
                format!(
                    "Thread {thread_id} has no engine holding it, so its session file cannot be {} now.",
                    if archived { "archived" } else { "unarchived" }
                ),
                "Open it with thread_create (tool.thread_create({ thread }) in an eval cell), then retry.",
                None,
            ));
        }
        LocatedControl::Control(c) => c,
    };
    if archived
        && control.device().is_none()
        && control.thread_id() == caller_id
        && control.running().await?
    {
        return Ok(failure(
            "turn_active",
            "This is the calling thread and its turn is still running; archiving it mid-turn is refused.",
            "Archive it from another thread, or after this turn ends.",
            Some(json!({ "thread": control.thread_id() })),
        ));
    }
    control.set_archived(archived).await?;
    let mut row = control.listed(archived).await?;
    if let Some(obj) = row.as_object_mut() {
        obj.insert("kind".into(), Value::String("ok".into()));
        obj.insert("archived".into(), Value::Bool(archived));
    }
    Ok(row)
}
