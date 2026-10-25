//! Durable summary files. A pending event is persisted before its window advances.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub const SUMMARY_TTL_MS: f64 = 14.0 * 24.0 * 60.0 * 60_000.0;
pub const OWNER_STALE_MS: f64 = 5.0 * 60_000.0;

pub fn new_summary_id() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}
pub fn new_summary_epoch() -> io::Result<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
pub fn current_process_owner() -> Value {
    static OWNER: OnceLock<Value> = OnceLock::new();
    OWNER.get_or_init(|| json!({"pid":std::process::id(),"bootId":new_summary_id().expect("process owner entropy")})).clone()
}
pub fn read_process_start_time(pid: i64) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        return stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)
            .map(str::to_owned);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}
pub fn is_record_sendable(record: &Value, consent: &dyn Fn(&Path) -> bool) -> bool {
    record["consent"] == "enabled"
        && record["scope"]["cwd"]
            .as_str()
            .is_some_and(|cwd| !cwd.is_empty() && consent(Path::new(cwd)))
}
pub fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
pub fn list_names(dir: &Path) -> io::Result<Vec<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}
pub fn read_json(path: &Path) -> io::Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).unwrap_or(Value::Null))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn write_json_atomic(
    path: &Path,
    value: &Value,
    owner: &Value,
    exclusive: bool,
) -> io::Result<bool> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("summary path has no parent"))?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let temp = PathBuf::from(format!(
        "{}.{}-{}.tmp",
        path.display(),
        owner["pid"],
        owner["bootId"].as_str().unwrap_or_default()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result: io::Result<bool> = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(value.to_string().as_bytes())?;
        file.sync_all()?;
        if exclusive {
            match fs::hard_link(&temp, path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        } else {
            fs::rename(&temp, path)?;
            Ok(true)
        }
    })();
    let cleanup = remove_file(&temp);
    let installed = result?;
    cleanup?;
    Ok(installed)
}
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-@".contains(&b))
}
pub fn safe_uuid(name: &str) -> bool {
    name.len() == 36
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}
pub fn parse_pending_record(value: Value) -> Option<Value> {
    (value["v"] == 2
        && valid_owner(&value["owner"])
        && value["uuid"].as_str().is_some_and(safe_uuid)
        && value["sessionHash"].is_string()
        && value["createdAt"].as_f64().is_some()
        && value["properties"].is_object())
    .then_some(value)
}
pub fn valid_owner(owner: &Value) -> bool {
    owner["pid"].as_i64().is_some() && owner["bootId"].as_str().is_some_and(|id| !id.is_empty())
}
