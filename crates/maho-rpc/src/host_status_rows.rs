//! The per-row detail of a status report (senpi `rpc/host-status-rows.ts`): the sessions a host listed and
//! the session-path claims its endpoint's `reservations/` directory holds, parsed at the boundary.
use crate::host_daemon_paths::HostDaemonPaths;
use crate::host_generations::HostGenerationRow;
use crate::host_reservations::{claim_owner_is_live, read_session_path_claims};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// The per-session heap split a host published (senpi#1960); zeros when none was published.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HostSessionRowMemory {
    pub main_heap_mb: f64,
    pub kernel_heap_mb: f64,
    pub kernel_count: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HostSessionRow {
    pub id: String,
    pub kind: String,
    pub session_path: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub attachments: f64,
    /// The labels published on a worker listing; `None` when the host published none.
    pub context: Option<BTreeMap<String, String>>,
    pub memory: HostSessionRowMemory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostPathClaimRow {
    pub session_path: String,
    pub owner_pid: u32,
    pub instance_id: String,
    /// The owner generation's ordinal, `None` once its generation record is gone.
    pub generation: Option<u64>,
    /// Whether the owner had a client attached when it last published; `None` for a pre-flag claim.
    pub attached: Option<bool>,
    pub live: bool,
}

fn string_field(entry: &Map<String, Value>, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub fn parse_session_rows(reply: &Value) -> Vec<HostSessionRow> {
    let Some(sessions) = reply
        .as_object()
        .and_then(|reply| reply.get("sessions"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| HostSessionRow {
            id: string_field(entry, "sessionId").unwrap_or_default(),
            kind: string_field(entry, "kind").unwrap_or_else(|| "interactive".into()),
            session_path: string_field(entry, "sessionPath"),
            cwd: string_field(entry, "cwd"),
            name: string_field(entry, "name"),
            attachments: entry
                .get("attachments")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            context: string_record(entry.get("context")),
            memory: parse_row_memory(entry.get("memory")),
        })
        .collect()
}

/// The wire block `{ main_heap_bytes, kernel_heap_bytes, kernel_count }` as megabytes; zeros when absent.
fn parse_row_memory(value: Option<&Value>) -> HostSessionRowMemory {
    let Some(value) = value.and_then(Value::as_object) else {
        return HostSessionRowMemory::default();
    };
    let finite = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite())
    };
    HostSessionRowMemory {
        main_heap_mb: finite("main_heap_bytes").map_or(0.0, |bytes| (bytes / BYTES_PER_MB).round()),
        kernel_heap_mb: finite("kernel_heap_bytes")
            .map_or(0.0, |bytes| (bytes / BYTES_PER_MB).round()),
        kernel_count: finite("kernel_count").unwrap_or(0.0),
    }
}

fn string_record(value: Option<&Value>) -> Option<BTreeMap<String, String>> {
    let object = value?.as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
            .collect(),
    )
}

pub fn read_claim_rows(
    paths: &HostDaemonPaths,
    generations: &[HostGenerationRow],
) -> Vec<HostPathClaimRow> {
    let mut rows: Vec<HostPathClaimRow> = read_session_path_claims(&paths.reservations_dir)
        .into_iter()
        .map(|claim| {
            let owner = claim.owner;
            HostPathClaimRow {
                generation: generations
                    .iter()
                    .find(|row| row.instance_id == owner.instance_id)
                    .map(|row| row.generation as u64),
                attached: owner.attached,
                live: claim_owner_is_live(&owner),
                session_path: owner.session_path,
                owner_pid: owner.pid,
                instance_id: owner.instance_id,
            }
        })
        .collect();
    rows.sort_by(|left, right| left.session_path.cmp(&right.session_path));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listing_rows_are_typed_with_defaults() {
        let rows = parse_session_rows(&json!({"sessions":[
            {"sessionId":"s1","kind":"worker","sessionPath":"/p","attachments":2,"context":{"owner":"a","n":1},"memory":{"main_heap_bytes":3145728,"kernel_count":2}},
            {},
            "junk"
        ]}));
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].context,
            Some(BTreeMap::from([("owner".to_owned(), "a".to_owned())]))
        );
        assert_eq!(
            rows[0].memory,
            HostSessionRowMemory {
                main_heap_mb: 3.0,
                kernel_heap_mb: 0.0,
                kernel_count: 2.0
            }
        );
        assert_eq!(
            (
                rows[1].id.as_str(),
                rows[1].kind.as_str(),
                rows[1].context.clone()
            ),
            ("", "interactive", None)
        );
        assert!(parse_session_rows(&json!([])).is_empty());
    }

    #[test]
    fn claims_are_read_sorted_with_their_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(dir.path());
        std::fs::create_dir_all(&paths.reservations_dir).expect("mkdir");
        for (name, path) in [("b", "/z"), ("a", "/a")] {
            std::fs::write(
                paths.reservations_dir.join(format!("{name}.json")),
                json!({"instanceId":"i1","pid":std::process::id(),"sessionPath":path}).to_string(),
            )
            .expect("write");
        }
        let generation = HostGenerationRow {
            instance_id: "i1".into(),
            generation: 4.0,
            pid: 1,
            engine_version: None,
            rss_mb: None,
            host_rss_mb: None,
            sessions: 0,
            current: true,
            alive: true,
        };
        let rows = read_claim_rows(&paths, &[generation]);
        assert_eq!(
            rows.iter()
                .map(|row| row.session_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/a", "/z"]
        );
        assert_eq!(
            (rows[0].generation, rows[0].attached, rows[0].live),
            (Some(4), None, true)
        );
    }
}
