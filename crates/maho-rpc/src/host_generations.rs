use crate::{
    host_daemon_paths::{HostDaemonPaths, generation_paths},
    host_daemon_state::{parse_json, read_file_or_undefined},
    host_process_metrics::read_host_process_metrics,
    host_reservations::{
        ClaimIdentity, SessionPathOwner, claim_owner_identity, process_is_live,
        read_session_path_claims,
    },
};
use serde::Serialize;
use std::{collections::HashSet, fs};
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostGenerationRow {
    pub instance_id: String,
    pub generation: f64,
    pub pid: u32,
    pub engine_version: Option<String>,
    /// Resident memory of that generation's process tree, `None` where the platform hides it.
    #[serde(rename = "rss_mb")]
    pub rss_mb: Option<i64>,
    /// Resident memory of that generation's own supervisor and host processes, without their tools.
    #[serde(rename = "host_rss_mb")]
    pub host_rss_mb: Option<i64>,
    pub sessions: usize,
    pub current: bool,
    pub alive: bool,
}
#[derive(Debug, Default, PartialEq)]
pub struct PrunedDaemonState {
    pub generations: Vec<String>,
    pub claims: Vec<String>,
    pub pointer: bool,
}
fn current_id(paths: &HostDaemonPaths) -> Option<String> {
    let raw = read_file_or_undefined(&paths.pointer_file).ok().flatten();
    parse_json(raw.as_deref())?
        .get("instance_id")?
        .as_str()
        .map(str::to_owned)
}
fn ids(paths: &HostDaemonPaths) -> Vec<String> {
    fs::read_dir(&paths.generations_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}
/// Senpi `PruneOptions`: `before_pointer_removal` is the deterministic race seam that runs after the pointer
/// was read and before it is removed.
#[derive(Default)]
pub struct PruneOptions<'a> {
    pub before_pointer_removal: Option<&'a dyn Fn()>,
}
pub fn prune_dead_generations(paths: &HostDaemonPaths) -> std::io::Result<PrunedDaemonState> {
    prune_dead_generations_with(paths, &PruneOptions::default())
}
pub fn prune_dead_generations_with(
    paths: &HostDaemonPaths,
    options: &PruneOptions<'_>,
) -> std::io::Result<PrunedDaemonState> {
    let pointer_text = read_file_or_undefined(&paths.pointer_file).ok().flatten();
    let pointed = parse_json(pointer_text.as_deref())
        .and_then(|pointer| pointer.get("instance_id")?.as_str().map(str::to_owned));
    let mut pruned = PrunedDaemonState::default();
    for id in ids(paths) {
        let generation = generation_paths(paths, &id);
        let raw = read_file_or_undefined(&generation.pid_file)?;
        let Some(record) = parse_json(raw.as_deref()) else {
            continue;
        };
        let Some(pid) = record
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
        else {
            continue;
        };
        if process_is_live(pid) {
            continue;
        }
        // A supervisor that died while its host child still runs is not an ended generation (#3054).
        if crate::host_stalled_evidence::host_child_alive(&generation, |line| {
            crate::host_supervisor_log::supervisor_log(&line)
        }) == Some(true)
        {
            continue;
        }
        pruned.generations.push(id);
    }
    // Claims end before the directories go: a prune cut short in between must not leave a claim whose generation
    // (and heartbeat) no longer exists to explain it.
    let ended: HashSet<String> = pruned.generations.iter().cloned().collect();
    pruned.claims = end_claims_of_ended_generations(paths, &ended);
    for id in &pruned.generations {
        let _ = fs::remove_dir_all(generation_paths(paths, id).dir);
    }
    // The pointer outliving its generation is what makes a client probe a socket nobody serves.
    let names_dead = pointed.is_some_and(|id| pruned.generations.contains(&id));
    pruned.pointer = match pointer_text {
        Some(read) if names_dead => {
            if let Some(hook) = options.before_pointer_removal {
                hook();
            }
            remove_pointer_if_unchanged(paths, &read)
        }
        _ => false,
    };
    Ok(pruned)
}
/// Removes the pointer only while it is still the one this prune read (senpi#1911). Prune runs outside the
/// ensure lock (`host status`), so a registration may have renamed a new pointer in since: the file is moved
/// aside atomically, compared, and a replacement is linked back unless an even newer one landed.
fn remove_pointer_if_unchanged(paths: &HostDaemonPaths, read: &str) -> bool {
    let parked = std::path::PathBuf::from(format!(
        "{}.prune-{}-{}.tmp",
        paths.pointer_file.display(),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    if fs::rename(&paths.pointer_file, &parked).is_err() {
        return false;
    }
    let unchanged = read_file_or_undefined(&parked).ok().flatten().as_deref() == Some(read);
    if !unchanged {
        let _ = fs::hard_link(&parked, &paths.pointer_file);
    }
    let _ = fs::remove_file(&parked);
    unchanged
}
/// Removes every claim whose owner is gone, and every claim of a generation in `ended` (being removed by the caller)
/// whose owner identity cannot be proven. Returns the session paths released. Every path that removes a generation
/// directory calls this first, so no generation leaves claims behind that its directory no longer explains.
pub fn end_claims_of_ended_generations(
    paths: &HostDaemonPaths,
    ended: &HashSet<String>,
) -> Vec<String> {
    let mut claims = Vec::new();
    for claim in read_session_path_claims(&paths.reservations_dir) {
        if !claim_ended(&claim.owner, ended) {
            continue;
        }
        let _ = fs::remove_file(claim.file);
        claims.push(claim.owner.session_path);
    }
    claims
}
/// A claim ends with its owner's pid, or with the generation just removed when the owner's identity cannot be
/// proven (desktop#3157). Such an owner might only be frozen, so its session file is fenced first; a fence that
/// cannot be written keeps the claim.
fn claim_ended(owner: &SessionPathOwner, ended: &HashSet<String>) -> bool {
    match claim_owner_identity(owner) {
        ClaimIdentity::Gone => true,
        ClaimIdentity::Proven => false,
        ClaimIdentity::Unproven => {
            ended.contains(&owner.instance_id) && advance_session_fence(&owner.session_path).is_ok()
        }
    }
}
/// Gives the session file a new write-fence token (`<session file>.fence`), so a writer that opened it before this
/// call and re-reads the token refuses its next write (senpi `advanceSessionFence`).
fn advance_session_fence(session_file: &str) -> std::io::Result<()> {
    let file = format!("{session_file}.fence");
    let temporary = format!(
        "{file}.{}.{}.tmp",
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let result = (|| {
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        std::io::Write::write_all(
            &mut options.open(&temporary)?,
            format!("{}\n", uuid::Uuid::new_v4()).as_bytes(),
        )?;
        fs::rename(&temporary, &file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
/// senpi `readGenerationRows` options: `include_dead` is for the reader that must not prune (`host status --all`),
/// which reports a dead record as an `alive: false` row instead of dropping it.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenerationRowOptions {
    pub include_dead: bool,
}
fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}
/// Every generation of this daemon that is still running (the supervisor's pid, or its host child), oldest first.
pub fn read_generation_rows(paths: &HostDaemonPaths) -> std::io::Result<Vec<HostGenerationRow>> {
    read_generation_rows_with(paths, GenerationRowOptions::default())
}
pub fn read_generation_rows_with(
    paths: &HostDaemonPaths,
    options: GenerationRowOptions,
) -> std::io::Result<Vec<HostGenerationRow>> {
    let current = current_id(paths);
    let claims = read_session_path_claims(&paths.reservations_dir);
    let mut rows = Vec::new();
    for instance_id in ids(paths) {
        let raw = read_file_or_undefined(&generation_paths(paths, &instance_id).pid_file)?;
        let Some(record) = parse_json(raw.as_deref()) else {
            continue;
        };
        let Some(pid) = record
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
        else {
            continue;
        };
        let alive = process_is_live(pid)
            || crate::host_stalled_evidence::host_child_alive(
                &generation_paths(paths, &instance_id),
                |line| crate::host_supervisor_log::supervisor_log(&line),
            ) == Some(true);
        if !alive && !options.include_dead {
            continue;
        }
        let metrics = if alive {
            read_host_process_metrics(pid, platform())
        } else {
            crate::host_process_metrics::HostProcessMetrics::default()
        };
        rows.push(HostGenerationRow {
            generation: record
                .get("generation")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.),
            pid,
            engine_version: record
                .get("engineVersion")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            rss_mb: metrics.rss_mb,
            host_rss_mb: metrics.host_rss_mb,
            sessions: claims
                .iter()
                .filter(|claim| claim.owner.instance_id == instance_id)
                .count(),
            current: current.as_ref() == Some(&instance_id),
            instance_id,
            alive,
        });
    }
    rows.sort_by(|a, b| a.generation.total_cmp(&b.generation));
    Ok(rows)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claims_of_an_ended_generation_end_when_unproven_and_proven_claims_stay() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let write = |name: &str, instance: &str, pid: u32, start: Option<String>| {
            let session = temp.path().join(name).to_string_lossy().into_owned();
            fs::write(crate::host_reservations::reservation_file(&paths.reservations_dir,&session),serde_json::json!({"instanceId":instance,"pid":pid,"processStartTime":start,"sessionPath":session}).to_string()).unwrap();
            session
        };
        let own = std::process::id();
        let proven = write(
            "proven.jsonl",
            "gen",
            own,
            crate::host_reservations::read_process_start_time(own),
        );
        let unproven = write("unproven.jsonl", "gen", own, None);
        let other = write("other.jsonl", "other", own, None);
        let gone = write("gone.jsonl", "gen", u32::MAX, None);
        let ended = std::collections::HashSet::from(["gen".to_owned()]);
        let mut released = end_claims_of_ended_generations(&paths, &ended);
        released.sort();
        let mut expected = vec![unproven.clone(), gone];
        expected.sort();
        assert_eq!(released, expected);
        assert!(
            !fs::read_to_string(format!("{unproven}.fence"))
                .unwrap()
                .trim()
                .is_empty()
        );
        let left = read_session_path_claims(&paths.reservations_dir)
            .into_iter()
            .map(|claim| claim.owner.session_path)
            .collect::<HashSet<_>>();
        assert_eq!(left, HashSet::from([proven, other]));
    }
    #[test]
    fn prune_preserves_live_and_malformed_records() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        for id in ["dead", "live", "partial"] {
            crate::host_daemon_paths::create_generation_directory(&generation_paths(&paths, id))
                .unwrap();
        }
        fs::write(generation_paths(&paths, "dead").pid_file, r#"{"pid":0}"#).unwrap();
        fs::write(
            generation_paths(&paths, "live").pid_file,
            serde_json::json!({"pid":std::process::id(),"generation":2}).to_string(),
        )
        .unwrap();
        fs::write(generation_paths(&paths, "partial").pid_file, "{").unwrap();
        fs::write(&paths.pointer_file, r#"{"instance_id":"dead"}"#).unwrap();
        let pruned = prune_dead_generations(&paths).unwrap();
        assert_eq!(pruned.generations, vec!["dead"]);
        assert!(pruned.pointer);
        assert!(generation_paths(&paths, "partial").dir.exists());
        let rows = read_generation_rows(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].instance_id, "live");
        assert!(!rows[0].current);
    }
    /// The child record of a generation whose supervisor died, naming this live process.
    fn live_child(paths: &HostDaemonPaths, id: &str) {
        let generation = generation_paths(paths, id);
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        fs::write(&generation.pid_file, r#"{"pid":0,"generation":1}"#).unwrap();
        let own = std::process::id();
        fs::write(
            &generation.child_pid_file,
            serde_json::json!({"pid":own,"processStartTime":crate::host_reservations::read_process_start_time(own)}).to_string(),
        )
        .unwrap();
    }
    // senpi#3054: a generation whose supervisor died but whose host child still runs is not pruned, and is alive.
    #[test]
    fn prune_keeps_a_generation_whose_host_child_is_alive_and_rows_report_it_alive() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        live_child(&paths, "child");
        fs::write(&paths.pointer_file, r#"{"instance_id":"child"}"#).unwrap();
        let pruned = prune_dead_generations(&paths).unwrap();
        assert!(pruned.generations.is_empty());
        assert!(!pruned.pointer);
        assert!(generation_paths(&paths, "child").pid_file.exists());
        let rows = read_generation_rows(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].alive && rows[0].current);
        // A dead child releases it.
        fs::write(
            generation_paths(&paths, "child").child_pid_file,
            serde_json::json!({"pid":u32::MAX}).to_string(),
        )
        .unwrap();
        assert_eq!(
            prune_dead_generations(&paths).unwrap().generations,
            vec!["child"]
        );
    }
    // senpi `readGenerationRows(paths, { includeDead })`: a dead record is an `alive: false` row with no memory.
    #[test]
    fn rows_include_dead_records_only_when_asked() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        for (id, pid, generation) in [("gone", 0, 0), ("live", std::process::id(), 1)] {
            crate::host_daemon_paths::create_generation_directory(&generation_paths(&paths, id))
                .unwrap();
            fs::write(
                generation_paths(&paths, id).pid_file,
                serde_json::json!({"pid":pid,"generation":generation}).to_string(),
            )
            .unwrap();
        }
        let default = read_generation_rows(&paths).unwrap();
        assert_eq!(
            default
                .iter()
                .map(|row| row.instance_id.as_str())
                .collect::<Vec<_>>(),
            vec!["live"]
        );
        let all =
            read_generation_rows_with(&paths, GenerationRowOptions { include_dead: true }).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].instance_id, "gone");
        assert!(!all[0].alive);
        assert_eq!((all[0].rss_mb, all[0].host_rss_mb), (None, None));
        assert!(all[1].alive);
        let json = serde_json::to_value(&all[0]).unwrap();
        assert!(
            json.get("host_rss_mb")
                .is_some_and(serde_json::Value::is_null)
        );
    }
    #[test]
    fn prune_keeps_a_pointer_a_registration_replaced_after_it_was_read() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        crate::host_daemon_paths::create_generation_directory(&generation_paths(&paths, "dead"))
            .unwrap();
        fs::write(generation_paths(&paths, "dead").pid_file, r#"{"pid":0}"#).unwrap();
        fs::write(&paths.pointer_file, r#"{"instance_id":"dead"}"#).unwrap();
        let replacement = r#"{"instance_id":"fresh"}"#;
        let replace = || fs::write(&paths.pointer_file, replacement).unwrap();
        let pruned = prune_dead_generations_with(
            &paths,
            &PruneOptions {
                before_pointer_removal: Some(&replace),
            },
        )
        .unwrap();
        assert_eq!(pruned.generations, vec!["dead"]);
        assert!(!pruned.pointer);
        assert_eq!(
            fs::read_to_string(&paths.pointer_file).unwrap(),
            replacement
        );
        let leftovers = fs::read_dir(&paths.dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".prune-"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
