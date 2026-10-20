use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

pub struct ClaimedCrashRecord {
    pub record: Value,
    pub source: &'static str,
    pub shard_kind: &'static str,
}
pub fn claim_unreported_crash_records(
    agent: &Path,
    state: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> io::Result<Vec<ClaimedCrashRecord>> {
    let dir = state.join("crash-reports");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let mut sources = vec![(
        "process-crashes".to_owned(),
        agent.join("process-crashes/crashes.jsonl"),
        "process",
        None,
    )];
    let endpoints = match fs::read_dir(agent.join("rpc-host-daemon")) {
        Ok(entries) => entries.collect::<io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut endpoints: Vec<_> = endpoints.into_iter().map(|e| e.path()).collect();
    endpoints.sort();
    for endpoint in endpoints {
        sources.push((
            format!(
                "rpc-host-daemon/{}",
                endpoint.file_name().unwrap_or_default().to_string_lossy()
            ),
            endpoint.join("crashes.jsonl"),
            "rpc-host",
            Some(endpoint),
        ));
    }
    let mut claimed = Vec::new();
    let mut live = BTreeSet::new();
    for (id, file, source, endpoint) in sources {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Some(record) = parse_crash_record(line) else {
                continue;
            };
            let fingerprint =
                format!("{:x}", Sha256::digest(format!("{id}\n{line}")))[..32].to_owned();
            live.insert(fingerprint.clone());
            let at =
                chrono::DateTime::parse_from_rfc3339(record["at"].as_str().unwrap_or_default())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if claimed.len() >= 20 || now.signed_duration_since(at).num_days() >= 14 {
                continue;
            }
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(dir.join(fingerprint)) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
            claimed.push(ClaimedCrashRecord {
                record,
                source,
                shard_kind: endpoint.as_deref().map_or("none", endpoint_shard_kind),
            });
        }
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if live.contains(&entry.file_name().to_string_lossy().into_owned()) {
            continue;
        }
        let modified: chrono::DateTime<chrono::Utc> = entry.metadata()?.modified()?.into();
        if now.signed_duration_since(modified).num_hours() > 24 {
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(claimed)
}
pub fn parse_crash_record(line: &str) -> Option<Value> {
    let record: Value = serde_json::from_str(line).ok()?;
    chrono::DateTime::parse_from_rfc3339(record["at"].as_str()?).ok()?;
    record["uptimeMs"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)?;
    Some(record)
}
fn endpoint_shard_kind(endpoint: &Path) -> &'static str {
    for file in ["endpoint.json", "settings.json"] {
        let text = match fs::read_to_string(endpoint.join(file)) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(socket) = value["socket"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let normalized = if cfg!(windows) {
            socket.replace('/', "\\").to_lowercase()
        } else {
            socket.to_owned()
        };
        let digest = format!("{:x}", Sha256::digest(normalized));
        if endpoint.file_name().and_then(|s| s.to_str()) != Some(&digest[..16]) {
            continue;
        }
        let path = PathBuf::from(socket);
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if let Some((kind, key)) = name.strip_suffix(".sock").and_then(|s| s.split_once('-'))
            && key.len() == 16
            && key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            match kind {
                "p" => return "p",
                "i" => return "i",
                _ => {}
            }
        }
        return "none";
    }
    "unknown"
}
