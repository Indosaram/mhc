use std::path::Path;

use crate::kernels::shared::{
    runtime_asset::require_codemode_runtime_asset,
    subprocess_process::{ProcessError, SubprocessProcess},
};

use super::{worker_host::WorkerHost, worker_startup::WorkerStartupOptions};

pub const PROCESS_STARTUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn spawn_process_worker(
    options: &WorkerStartupOptions<'_>,
) -> Result<WorkerHost, ProcessError> {
    let entry = match options.worker_entry {
        Some(entry) => entry.to_path_buf(),
        None => require_codemode_runtime_asset(None, Path::new("kernels/js/process-entry.js"))
            .map_err(|error| ProcessError::Startup(error.to_string()))?,
    };
    let mut env = maho_ext_api::process_env::vars()
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    env.insert(
        "SENPI_CODEMODE_PROCESS_POOL_WIDTH".into(),
        options.parallel_pool_width.to_string(),
    );
    env.insert(
        "SENPI_CODEMODE_PROCESS_CWD".into(),
        options.cwd.to_string_lossy().into_owned(),
    );
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut process = SubprocessProcess::spawn_with_frame_token(
        "bun",
        &[entry.to_string_lossy().into_owned()],
        options.cwd,
        &env,
        Some(token.clone()),
    )?;
    process.send_token(&token).await?;
    Ok(WorkerHost::from_process(process))
}

pub fn decode_process_frame(line: &str, token: &str) -> serde_json::Value {
    use serde_json::json;
    let prefix = format!("{token} ");
    let Some(frame) = line.strip_prefix(&prefix) else {
        return json!({"type":"text","stream":"stdout","data":format!("{line}\n")});
    };
    let decoded =
        crate::bridge::protocol::parse_bridge_json_line(frame, None).and_then(|mut value| {
            revive_frame_values(&mut value, token);
            crate::bridge::protocol::decode_bridge_frame(&value.to_string(), None)
        });
    decoded.unwrap_or_else(|error| json!({"type":"init-failed","error":{"message":format!("JavaScript kernel process emitted an invalid frame: {error}")}}))
}

pub fn crash_cause_from(stderr: &str, token: &str) -> Option<serde_json::Value> {
    let prefix = format!("senpi-kernel-crash {token} ");
    let line = stderr.lines().find_map(|line| line.strip_prefix(&prefix))?;
    let cause: serde_json::Value = serde_json::from_str(line).ok()?;
    cause["message"].as_str()?;
    Some(cause)
}

fn revive_frame_values(value: &mut serde_json::Value, token: &str) {
    match value {
        serde_json::Value::Object(fields) => {
            fields.retain(|_, value| {
                value.as_object().is_none_or(|object| {
                    !(object.len() == 1
                        && object.contains_key(&format!("\0senpi:undefined:{token}")))
                })
            });
            for value in fields.values_mut() {
                revive_frame_values(value, token);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                revive_frame_values(value, token);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}
