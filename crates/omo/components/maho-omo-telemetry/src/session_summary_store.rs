use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct SummaryStore {
    dir: PathBuf,
}
impl SummaryStore {
    pub fn new(state: &Path) -> Self {
        Self {
            dir: state.join("rollups"),
        }
    }
    fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json"))
    }
    pub fn read(&self, hash: &str) -> io::Result<Option<Value>> {
        read_rollup(&self.path(hash))
    }
    pub fn write(&self, rollup: &Value) -> io::Result<()> {
        let hash = rollup["sessionHash"]
            .as_str()
            .filter(|s| safe_hash(s))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid rollup identity"))?;
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        let target = self.path(hash);
        let temp = target.with_extension(format!("json.{}.tmp", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options
            .open(&temp)?
            .write_all(rollup.to_string().as_bytes())?;
        fs::rename(temp, target)
    }
    pub fn remove(&self, hash: &str) -> io::Result<()> {
        remove(&self.path(hash))
    }
    pub fn claim_orphans(&self, now: f64) -> io::Result<Vec<Value>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut claimed = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let owner = name
                .rsplit_once(".claimed-")
                .and_then(|(_, pid)| pid.parse::<i64>().ok())
                .or_else(|| {
                    name.strip_suffix(".tmp")
                        .and_then(|s| s.rsplit_once('.'))
                        .and_then(|(_, pid)| pid.parse::<i64>().ok())
                });
            if let Some(pid) = owner {
                if pid != i64::from(std::process::id())
                    && !senpi_task::dag::store::default_is_process_alive(pid)
                {
                    remove(&path)?;
                }
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let rollup = read_rollup(&path)?;
            if let Some(rollup) = &rollup {
                if now - rollup["updatedAt"].as_f64().unwrap_or(0.0) > 14.0 * 24.0 * 60.0 * 60_000.0
                {
                    remove(&path)?;
                    continue;
                }
                let pid = rollup["pid"].as_i64().unwrap_or(0);
                if pid == i64::from(std::process::id())
                    || senpi_task::dag::store::default_is_process_alive(pid)
                {
                    continue;
                }
            }
            let claim = path.with_extension(format!("json.claimed-{}", std::process::id()));
            match fs::rename(&path, &claim) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
            let owned = read_rollup(&claim)?;
            remove(&claim)?;
            if let Some(owned) = owned {
                claimed.push(owned);
            }
        }
        Ok(claimed)
    }
}
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
fn safe_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read_rollup(path: &Path) -> io::Result<Option<Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
        return Ok(None);
    };
    if value["v"] != 1
        || value["pid"].as_i64().is_none()
        || !value["sessionHash"].as_str().is_some_and(safe_hash)
        || value["nextSeq"].as_u64().is_none()
        || value["updatedAt"].as_f64().is_none()
    {
        return Ok(None);
    }
    if !matches!(
        value["sessionRole"].as_str(),
        Some("main" | "child" | "dag_child" | "member")
    ) {
        return Ok(None);
    }
    if !value["window"].is_null() {
        let Some(window) = crate::session_summary_window::parse_window(&value["window"]) else {
            return Ok(None);
        };
        value["window"] = window;
    }
    Ok(Some(value))
}
pub fn rollup(hash: &str, seq: u64, window: Value, cwd: &Path, now: f64, role: &str) -> Value {
    json!({"v":1,"pid":std::process::id(),"sessionHash":hash,"sessionRole":role,"nextSeq":seq,"window":window,"updatedAt":now,"consent":"enabled","scope":{"cwd":cwd}})
}
