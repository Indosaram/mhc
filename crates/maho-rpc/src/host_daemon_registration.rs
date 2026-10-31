use crate::{
    host_daemon_paths::{HostDaemonPaths, generation_paths},
    host_daemon_state::{parse_json, read_file_or_undefined},
    host_reservations::read_process_start_time,
};
use serde_json::Value;
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredHost {
    pub pid: u32,
    pub process_start_time: Option<String>,
    pub socket: Option<String>,
    pub instance_id: String,
    pub generation: f64,
    pub writer: Option<Value>,
}
pub fn read_host_registration(paths: &HostDaemonPaths) -> std::io::Result<Option<RegisteredHost>> {
    let pointer = read_file_or_undefined(&paths.pointer_file)?;
    let Some(pointer) = parse_json(pointer.as_deref()) else {
        return Ok(None);
    };
    let Some(instance_id) = pointer.get("instance_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    read_generation_registration(paths, instance_id)
}
/// One NAMED generation's record, whatever the pointer says: a caller asking a superseded generation to drain
/// addresses it by instance id (senpi#3653). The record proves nothing by itself; `proven_owner` still decides
/// whether it names a live process serving this endpoint (senpi `readGenerationRegistration`).
pub fn read_generation_registration(
    paths: &HostDaemonPaths,
    instance_id: &str,
) -> std::io::Result<Option<RegisteredHost>> {
    let text = read_file_or_undefined(&generation_paths(paths, instance_id).pid_file)?;
    let Some(record) = parse_json(text.as_deref()) else {
        return Ok(None);
    };
    let Some(pid) = record
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    else {
        return Ok(None);
    };
    let process_start_time = match record.get("processStartTime") {
        Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => return Ok(None),
    };
    let writer=record.get("writer").filter(|writer|writer.is_object()&&writer["pid"].is_number()).map(|writer|serde_json::json!({"pid":writer["pid"],"startTime":writer["startTime"].as_str()}));
    Ok(Some(RegisteredHost {
        pid,
        process_start_time,
        socket: record
            .get("socket")
            .and_then(Value::as_str)
            .map(str::to_owned),
        instance_id: instance_id.into(),
        generation: record
            .get("generation")
            .and_then(Value::as_f64)
            .unwrap_or(0.),
        writer,
    }))
}
/// A caller-supplied instance id names a directory under `generations/`: never a path of its own
/// (`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$` and no `..`; senpi `isGenerationId`).
pub fn is_generation_id(instance_id: &str) -> bool {
    let bytes = instance_id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !instance_id.contains("..")
}
pub fn proven_owner<'a>(
    registered: Option<&'a RegisteredHost>,
    socket: &str,
) -> Option<&'a RegisteredHost> {
    let registered = registered?;
    if registered
        .socket
        .as_deref()
        .is_some_and(|recorded| recorded != socket)
    {
        return None;
    }
    let recorded = registered.process_start_time.as_deref()?;
    (read_process_start_time(registered.pid).as_deref() == Some(recorded)).then_some(registered)
}
pub fn written_by_this_process(writer: Option<&Value>) -> bool {
    let Some(writer) = writer else {
        return false;
    };
    writer["pid"].as_u64() == Some(u64::from(std::process::id()))
        && writer["startTime"].as_str().is_some_and(|recorded| {
            read_process_start_time(std::process::id()).as_deref() == Some(recorded)
        })
}
pub struct HostRegistration {
    pub pid: u32,
    pub process_start_time: Option<String>,
    pub socket: String,
    pub instance_id: String,
    pub generation: f64,
    pub launch_profile_id: String,
    /// The build of the process this record names. `None` while it is not known yet (a handoff's successor before
    /// it answers): the record then claims no engine version rather than the writer's (senpi `HostRegistration.build`).
    pub build: Option<RegisteredBuild>,
}
/// An engine build as a record states it: `engineVersion` and its five-part `engineOrdinal`.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredBuild {
    pub text: String,
    pub ordinal: [Value; 5],
}
impl RegisteredBuild {
    /// THIS process's build: for a record naming a process this binary started itself.
    pub fn this_engine() -> Self {
        let build = maho_core::engine_build_identity::engine_build_identity();
        Self {
            text: build.text.clone(),
            ordinal: build.ordinal.map(Value::from),
        }
    }
    /// The build a host reported about itself on its socket (senpi `successorBuild`); `None` when it named none.
    pub fn answered(host: &crate::host_protocol_info::HostProtocolInfo) -> Option<Self> {
        let text = host.engine_version.clone()?;
        let ordinal = host.engine_ordinal?.map(|part| {
            if part.fract() == 0.0 && part.abs() <= 9_007_199_254_740_991.0 {
                Value::from(part as i64)
            } else {
                Value::from(part)
            }
        });
        Some(Self { text, ordinal })
    }
}
/// The process that wrote a pidfile record (senpi `HostPidFileWriter`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPidFileWriter {
    pub pid: u32,
    pub start_time: Option<String>,
}
/// Writes one generation's `host.pid` record (senpi `writeGenerationRecord`), creating its directory.
pub fn write_generation_record(
    paths: &HostDaemonPaths,
    registration: &HostRegistration,
) -> Result<
    (
        crate::host_daemon_paths::HostGenerationPaths,
        HostPidFileWriter,
    ),
    crate::host_daemon_paths::HostDaemonStateError,
> {
    let generation = generation_paths(paths, &registration.instance_id);
    crate::host_daemon_paths::create_generation_directory(&generation)?;
    let writer = HostPidFileWriter {
        pid: std::process::id(),
        start_time: read_process_start_time(std::process::id()),
    };
    let mut record = serde_json::json!({"pid":registration.pid,"processStartTime":registration.process_start_time,"instance_id":registration.instance_id,"generation":registration.generation,"launchProfileId":registration.launch_profile_id,"socket":registration.socket,"writer":{"pid":writer.pid,"startTime":writer.start_time}});
    if let Some(build) = &registration.build {
        record["engineVersion"] = build.text.clone().into();
        record["engineOrdinal"] = Value::Array(build.ordinal.to_vec());
    }
    crate::host_daemon_state::write_state_file(&generation.pid_file, &record)?;
    Ok((generation, writer))
}
/// Drops the pointer and settings when they still name `instance_id` (senpi `releaseRegistrationPointer`). The
/// pointer goes LAST: "no pointer" is what every reader takes as "no host here" (senpi#2241).
pub fn release_registration_pointer(
    paths: &HostDaemonPaths,
    instance_id: &str,
) -> std::io::Result<()> {
    let pointer = parse_json(read_file_or_undefined(&paths.pointer_file)?.as_deref());
    if pointer
        .as_ref()
        .and_then(|pointer| pointer.get("instance_id"))
        .and_then(Value::as_str)
        != Some(instance_id)
    {
        return Ok(());
    }
    remove_file(&paths.settings_file)?;
    remove_file(&paths.pointer_file)
}
pub fn write_host_registration(
    paths: &HostDaemonPaths,
    registration: &HostRegistration,
) -> Result<(), crate::host_daemon_paths::HostDaemonStateError> {
    crate::host_generations::prune_dead_generations(paths).map_err(|source| {
        crate::host_daemon_paths::HostDaemonStateError {
            path: paths.dir.clone(),
            source,
        }
    })?;
    let (generation, writer) = write_generation_record(paths, registration)?;
    let writer = serde_json::json!({"pid":writer.pid,"startTime":writer.start_time});
    let temporary = std::path::PathBuf::from(format!(
        "{}.{}.tmp",
        paths.pointer_file.display(),
        std::process::id()
    ));
    crate::host_daemon_state::write_state_file(
        &temporary,
        &serde_json::json!({"layout":crate::host_daemon_paths::HOST_DAEMON_LAYOUT,"instance_id":registration.instance_id,"generation_dir":generation.relative_dir,"writer":writer}),
    )?;
    std::fs::rename(&temporary, &paths.pointer_file).map_err(|source| {
        crate::host_daemon_paths::HostDaemonStateError {
            path: paths.pointer_file.clone(),
            source,
        }
    })
}
pub fn release_generation(
    paths: &HostDaemonPaths,
    instance_id: &str,
    pid: u32,
) -> std::io::Result<()> {
    let generation = generation_paths(paths, instance_id);
    let record = read_file_or_undefined(&generation.pid_file)?;
    let record = parse_json(record.as_deref());
    if record
        .as_ref()
        .and_then(|record| record["pid"].as_u64())
        .is_some_and(|recorded| recorded != u64::from(pid))
    {
        return Ok(());
    }
    let pointer = read_file_or_undefined(&paths.pointer_file)?;
    let pointer = parse_json(pointer.as_deref());
    if pointer
        .as_ref()
        .and_then(|pointer| pointer["instance_id"].as_str())
        == Some(instance_id)
    {
        remove_file(&paths.pointer_file)?;
        remove_file(&paths.settings_file)?;
    }
    match std::fs::remove_dir_all(&generation.dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
fn remove_file(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
static SELF_START_TIME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
/// The process-start stamp of THIS process, read once (senpi `thisProcessStartTime`).
pub fn this_process_start_time() -> Option<String> {
    SELF_START_TIME
        .get_or_init(|| read_process_start_time(std::process::id()))
        .clone()
}
/// A registration written before the generation layout existed, or nothing (senpi
/// `readLegacyHostRecord`). It carries `pid`/`processStartTime`, so it can authorize a signal.
pub fn read_legacy_host_record(paths: &HostDaemonPaths) -> Option<Value> {
    read_file_or_undefined(&paths.legacy_pid_file)
        .ok()
        .flatten()
        .and_then(|text| parse_json(Some(&text)))
        .map(Value::Object)
}
/// Whether the legacy record names a live process whose start time still matches
/// (senpi `legacyHostIsLive`).
pub fn legacy_host_is_live(paths: &HostDaemonPaths) -> bool {
    let Some(record) = read_legacy_host_record(paths) else {
        return false;
    };
    let Some(pid) = record
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
    else {
        return false;
    };
    let recorded = record.get("processStartTime").and_then(Value::as_str);
    // senpi `processMatchesPidFile` defaults to `processIsRunning`: an unreaped zombie is not a live host.
    crate::host_reservations::process_is_running(pid)
        && recorded.is_none_or(|recorded| {
            crate::host_reservations::read_process_start_time(pid).as_deref() == Some(recorded)
        })
}
pub fn clear_host_registration(paths: &HostDaemonPaths) -> std::io::Result<()> {
    let pointer = read_file_or_undefined(&paths.pointer_file)?;
    if let Some(instance_id) = parse_json(pointer.as_deref())
        .as_ref()
        .and_then(|pointer| pointer.get("instance_id"))
        .and_then(Value::as_str)
    {
        // A cleared dead generation's unproven session claims end behind the write fence BEFORE its directory goes.
        crate::host_generations::end_claims_of_ended_generations(
            paths,
            &std::collections::HashSet::from([instance_id.to_owned()]),
        );
        match std::fs::remove_dir_all(generation_paths(paths, instance_id).dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            result => result?,
        }
    }
    remove_file(&paths.pointer_file)?;
    remove_file(&paths.settings_file)
}
#[cfg(test)]
mod tests {
    use super::*;
    // senpi host-successor.ts `successorBuild`: a successor's record claims the build it answered with, and no
    // build at all before it answered - never the writing process's.
    #[test]
    fn a_record_states_the_named_build_or_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = crate::host_daemon_paths::create_host_daemon_paths("s", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).expect("dirs");
        let answer = crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[],"instanceId":"next","engineVersion":"2099.1.2-3","engineOrdinal":[2099,1,2,3,4]})).expect("answer");
        let mut registration = HostRegistration {
            pid: 7,
            process_start_time: None,
            socket: "s".into(),
            instance_id: "next".into(),
            generation: 1.0,
            launch_profile_id: String::new(),
            build: None,
        };
        let (generation, _) = write_generation_record(&paths, &registration).expect("record");
        let read = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&generation.pid_file).expect("read"))
                .expect("json")
        };
        let unknown = read();
        assert!(unknown.get("engineVersion").is_none() && unknown.get("engineOrdinal").is_none());
        registration.build = RegisteredBuild::answered(&answer);
        write_generation_record(&paths, &registration).expect("record");
        let answered = read();
        assert_eq!(answered["engineVersion"], "2099.1.2-3");
        assert_eq!(
            answered["engineOrdinal"],
            serde_json::json!([2099, 1, 2, 3, 4])
        );
        let unnamed = crate::host_protocol_info::parse_host_protocol_info(
            &serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[]}),
        )
        .expect("answer");
        assert_eq!(RegisteredBuild::answered(&unnamed), None);
    }
    #[test]
    fn generation_records_and_pointer_release() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = crate::host_daemon_paths::create_host_daemon_paths("s", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).expect("dirs");
        let registration = HostRegistration {
            pid: 7,
            process_start_time: Some("1".into()),
            socket: "s".into(),
            instance_id: "g1".into(),
            generation: 3.0,
            launch_profile_id: "p".into(),
            build: None,
        };
        let (generation, writer) = write_generation_record(&paths, &registration).expect("record");
        assert_eq!(writer.pid, std::process::id());
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(&generation.pid_file).expect("read"))
                .expect("json");
        assert_eq!(
            (record["pid"].as_u64(), record["instance_id"].as_str()),
            (Some(7), Some("g1"))
        );
        std::fs::write(&paths.pointer_file, r#"{"instance_id":"other"}"#).expect("pointer");
        std::fs::write(&paths.settings_file, "{}").expect("settings");
        release_registration_pointer(&paths, "g1").expect("foreign kept");
        assert!(paths.pointer_file.exists() && paths.settings_file.exists());
        std::fs::write(&paths.pointer_file, r#"{"instance_id":"g1"}"#).expect("pointer");
        release_registration_pointer(&paths, "g1").expect("released");
        assert!(!paths.pointer_file.exists() && !paths.settings_file.exists());
    }
    #[test]
    fn legacy_record_is_read_and_liveness_requires_the_recorded_start_time() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        assert!(read_legacy_host_record(&paths).is_none());
        assert!(!legacy_host_is_live(&paths));
        let pid = std::process::id();
        std::fs::write(
            &paths.legacy_pid_file,
            serde_json::json!({"pid":pid,"processStartTime":read_process_start_time(pid)})
                .to_string(),
        )
        .unwrap();
        assert!(legacy_host_is_live(&paths));
        std::fs::write(
            &paths.legacy_pid_file,
            serde_json::json!({"pid":pid,"processStartTime":"other"}).to_string(),
        )
        .unwrap();
        assert!(!legacy_host_is_live(&paths));
        assert_eq!(this_process_start_time(), read_process_start_time(pid));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_legacy_host_is_not_live() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let child = crate::host_reservations::ZombieChild::spawn();
        let pid = child.id();
        std::fs::write(
            &paths.legacy_pid_file,
            serde_json::json!({"pid":pid,"processStartTime":read_process_start_time(pid)})
                .to_string(),
        )
        .unwrap();
        assert!(!legacy_host_is_live(&paths));
    }
    #[test]
    fn registration_requires_guard_shape_and_owner_matches_live_process() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let generation = generation_paths(&paths, "one");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        std::fs::write(&paths.pointer_file, r#"{"instance_id":"one"}"#).unwrap();
        assert!(read_host_registration(&paths).unwrap().is_none());
        let pid = std::process::id();
        let record = serde_json::json!({"pid":pid,"processStartTime":read_process_start_time(pid),"socket":"socket","generation":3});
        std::fs::write(&generation.pid_file, record.to_string()).unwrap();
        let registered = read_host_registration(&paths).unwrap().unwrap();
        assert_eq!(registered.generation, 3.);
        assert!(proven_owner(Some(&registered), "socket").is_some());
        assert!(proven_owner(Some(&registered), "other").is_none());
        let mut stale = registered;
        stale.process_start_time = Some("stale".into());
        assert!(proven_owner(Some(&stale), "socket").is_none());
        std::fs::write(
            &generation.pid_file,
            serde_json::json!({"pid":pid,"processStartTime":null}).to_string(),
        )
        .unwrap();
        let unguarded = read_host_registration(&paths).unwrap().unwrap();
        assert!(proven_owner(Some(&unguarded), "socket").is_none());
    }
    #[test]
    fn generation_ids_reject_traversal_and_bad_leading_characters() {
        assert!(is_generation_id("a1b2-c3.d_e"));
        assert!(!is_generation_id(""));
        assert!(!is_generation_id("-a"));
        assert!(!is_generation_id("a..b"));
        assert!(!is_generation_id("a/b"));
        assert!(!is_generation_id(&"a".repeat(129)));
        assert!(is_generation_id(&"a".repeat(128)));
    }
    #[test]
    fn a_named_generation_is_read_whatever_the_pointer_says() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let generation = generation_paths(&paths, "old");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        std::fs::write(
            &generation.pid_file,
            serde_json::json!({"pid":5,"processStartTime":null,"generation":1}).to_string(),
        )
        .unwrap();
        std::fs::write(&paths.pointer_file, r#"{"instance_id":"new"}"#).unwrap();
        assert!(read_host_registration(&paths).unwrap().is_none());
        let named = read_generation_registration(&paths, "old")
            .unwrap()
            .unwrap();
        assert_eq!((named.pid, named.instance_id.as_str()), (5, "old"));
    }
    #[test]
    fn clearing_a_registration_ends_the_unproven_claims_of_its_generation() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        crate::host_daemon_paths::create_generation_directory(&generation_paths(&paths, "gen"))
            .unwrap();
        std::fs::write(&paths.pointer_file, r#"{"instance_id":"gen"}"#).unwrap();
        let session = temp.path().join("s.jsonl").to_string_lossy().into_owned();
        let claim = crate::host_reservations::reservation_file(&paths.reservations_dir, &session);
        std::fs::write(&claim,serde_json::json!({"instanceId":"gen","pid":std::process::id(),"processStartTime":null,"sessionPath":session}).to_string()).unwrap();
        clear_host_registration(&paths).unwrap();
        assert!(!claim.exists());
        assert!(!generation_paths(&paths, "gen").dir.exists());
        assert!(
            !std::fs::read_to_string(format!("{session}.fence"))
                .unwrap()
                .trim()
                .is_empty()
        );
    }
    #[test]
    fn predecessor_release_preserves_successor_pointer_and_settings() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("socket", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let generation = generation_paths(&paths, "old");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        std::fs::write(&generation.pid_file, r#"{"pid":1}"#).unwrap();
        std::fs::write(&paths.pointer_file, r#"{"instance_id":"new"}"#).unwrap();
        std::fs::write(&paths.settings_file, "{}").unwrap();
        release_generation(&paths, "old", 2).unwrap();
        assert!(generation.dir.exists());
        release_generation(&paths, "old", 1).unwrap();
        assert!(!generation.dir.exists());
        assert!(paths.pointer_file.exists());
        assert!(paths.settings_file.exists());
    }
    #[test]
    fn writer_requires_current_process_start_guard() {
        let pid = std::process::id();
        assert!(!written_by_this_process(Some(
            &serde_json::json!({"pid":pid,"startTime":null})
        )));
        assert!(written_by_this_process(Some(
            &serde_json::json!({"pid":pid,"startTime":read_process_start_time(pid)})
        )));
    }
}
