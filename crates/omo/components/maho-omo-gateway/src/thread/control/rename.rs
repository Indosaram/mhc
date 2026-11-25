//! `thread_rename` body (`thread/control/rename.ts`).

use serde_json::Value;

use super::super::addressing::normalize_thread_name;
use super::super::contracts::params::ThreadRenameInput;
use super::super::tools::internals::failure;
use super::super::tools::ports::ThreadHostView;
use super::super::tools::ports::ThreadToolSurfaceOptions;
use super::locate::{LocatedControl, locate_control};
use super::operations::not_live_failure;
use super::port::{ControlError, control_identity};

pub async fn rename_thread(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    value: &ThreadRenameInput,
    _key: &str,
    caller_id: &str,
) -> Result<Value, ControlError> {
    if value.name.is_none() && value.alias.is_none() {
        return Ok(failure(
            "invalid_arguments",
            "Pass a new name, a new alias, or both.",
            "Retry with name or alias.",
            None,
        ));
    }
    let address = value.thread.as_deref().unwrap_or("self");
    let located = locate_control(
        options,
        current,
        address,
        value.all_scope.unwrap_or(false),
        caller_id,
    )
    .await;

    let control = match located {
        LocatedControl::Error(err) => return Ok(err),
        LocatedControl::NotLive { .. } => return Ok(not_live_failure()),
        LocatedControl::Control(ctrl) => ctrl,
    };

    let check_name = |name: &str,
                      visible: &[super::super::contracts::results::ThreadSummary]|
     -> Option<Value> {
        if name.is_empty() {
            return Some(failure(
                "invalid_arguments",
                "The new thread name is empty.",
                "Pass a non-empty name.",
                None,
            ));
        }
        let target_norm = normalize_thread_name(name);
        if let Some(existing) = visible.iter().find(|entry| {
            entry.thread_id != control.thread_id()
                && normalize_thread_name(&entry.name) == target_norm
        }) {
            return Some(failure(
                "name_conflict",
                format!("A thread named \"{}\" already exists.", existing.name),
                "Call thread_list (tool.thread_list() in an eval cell) and choose another name.",
                None,
            ));
        }
        None
    };

    let visible = control.visible().await?;
    let mut name = value.name.as_deref().map(str::trim).map(str::to_owned);
    if let Some(ref n) = name
        && let Some(err) = check_name(n, &visible)
    {
        return Ok(err);
    }

    let mut local_alias = false;
    if let Some(alias) = value.alias.as_deref() {
        // O01: account-wide alias registry is O02; treat alias locally as name fallback when no name was provided.
        local_alias = true;
        if name.is_none() {
            let trimmed = alias.trim().to_owned();
            if let Some(err) = check_name(&trimmed, &visible) {
                return Ok(err);
            }
            name = Some(trimmed);
        }
    }

    if let Some(ref n) = name {
        control.set_name(n).await?;
    }

    let mut obj = control_identity(control.thread_id(), control.device())
        .as_object()
        .cloned()
        .unwrap_or_default();
    obj.insert("kind".into(), Value::String("ok".into()));
    obj.insert(
        "name".into(),
        Value::String(name.unwrap_or_else(|| control.title().to_owned())),
    );
    if local_alias {
        obj.insert("alias_scope".into(), Value::String("local".into()));
        obj.insert(
            "alias_unavailable".into(),
            Value::String("remote_unavailable".into()),
        );
    }
    Ok(Value::Object(obj))
}
