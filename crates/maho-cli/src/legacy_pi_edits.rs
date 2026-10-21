use crate::{
    legacy_dir_copy::paths_point_to_same_location,
    migrations_state::{read_legacy_pi_agent_dir_record, write_legacy_pi_agent_dir_record},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const LEGACY_PI_CONFIG_FILES: [&str; 4] = [
    "auth.json",
    "keybindings.json",
    "models.json",
    "settings.json",
];
pub struct LegacyPiEdit {
    pub file: &'static str,
    pub pi_path: PathBuf,
    pub agent_path: PathBuf,
    pub mtime_ms: f64,
}
pub struct LegacyPiImported {
    pub file: String,
    pub from: PathBuf,
    pub to: PathBuf,
    pub backup_path: Option<PathBuf>,
}
pub struct LegacyPiImportFailure {
    pub file: String,
    pub reason: String,
}
pub struct LegacyPiImportResult {
    pub pi_agent_dir: PathBuf,
    pub agent_dir: PathBuf,
    pub imported: Vec<LegacyPiImported>,
    pub failures: Vec<LegacyPiImportFailure>,
}

pub fn mtime_ms(time: SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64() * 1000.0,
        Err(error) => -error.duration().as_secs_f64() * 1000.0,
    }
}

fn separate(agent: &Path, pi: &Path) -> io::Result<bool> {
    Ok(maho_core::config::config_dir_name() != ".pi"
        && pi.exists()
        && !paths_point_to_same_location(agent, pi)?)
}
fn file_stat(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(stat) => Ok(stat.is_file().then_some(stat)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
pub fn find_legacy_pi_edits(agent: &Path, home: &Path) -> io::Result<Vec<LegacyPiEdit>> {
    let pi = home.join(".pi/agent");
    if !separate(agent, &pi)? {
        return Ok(Vec::new());
    }
    let record = read_legacy_pi_agent_dir_record(agent);
    let mut edits = Vec::new();
    for file in LEGACY_PI_CONFIG_FILES {
        let pi_path = pi.join(file);
        let agent_path = agent.join(file);
        let Some(pi_stat) = file_stat(&pi_path)? else {
            continue;
        };
        let agent_stat = file_stat(&agent_path)?;
        let baseline = match record.copied_at {
            Some(time) => time,
            None => match &agent_stat {
                Some(stat) => mtime_ms(stat.modified()?),
                None => 0.0,
            },
        };
        let mtime_ms = mtime_ms(pi_stat.modified()?);
        if mtime_ms <= baseline {
            continue;
        }
        if agent_stat.is_some() && fs::read(&pi_path)? == fs::read(&agent_path)? {
            continue;
        }
        edits.push(LegacyPiEdit {
            file,
            pi_path,
            agent_path,
            mtime_ms,
        });
    }
    Ok(edits)
}
pub fn take_legacy_pi_edit_notice(agent: &Path, home: &Path) -> io::Result<Vec<LegacyPiEdit>> {
    let mut record = read_legacy_pi_agent_dir_record(agent);
    let edits: Vec<_> = find_legacy_pi_edits(agent, home)?
        .into_iter()
        .filter(|edit| record.noticed_mtimes.get(edit.file).copied() != Some(edit.mtime_ms))
        .collect();
    if !edits.is_empty() {
        record.noticed_mtimes.extend(
            edits
                .iter()
                .map(|edit| (edit.file.to_owned(), edit.mtime_ms)),
        );
        write_legacy_pi_agent_dir_record(&record, agent)?;
    }
    Ok(edits)
}
pub fn legacy_pi_edit_startup_notice(agent: &Path, home: &Path) -> Option<String> {
    match take_legacy_pi_edit_notice(agent, home) {
        Ok(edits) if edits.is_empty() => None,
        Ok(edits) => {
            let files = edits
                .iter()
                .map(|edit| edit.file)
                .collect::<Vec<_>>()
                .join(" ");
            let paths = edits
                .iter()
                .map(|edit| edit.pi_path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let app = maho_core::config::app_name();
            if let [only] = edits.as_slice() {
                return Some(format!(
                    "You edited {} after {app} moved to {}; {app} reads {}. Copy your change there (or run: {} config import-pi {files}).",
                    only.pi_path.display(),
                    agent.display(),
                    only.agent_path.display(),
                    maho_core::config::app_command()
                ));
            }
            Some(format!(
                "You edited {paths} after {app} moved to {}; {app} reads its config from {}. Copy your changes there (or run: {} config import-pi {files}).",
                agent.display(),
                agent.display(),
                maho_core::config::app_command()
            ))
        }
        Err(error) => Some(format!(
            "Could not check {} for config edits: {error}",
            home.join(".pi/agent").display()
        )),
    }
}

fn backup_suffix(now: SystemTime) -> io::Result<String> {
    let seconds = now.duration_since(UNIX_EPOCH).map_err(io::Error::other)?;
    let days = i64::try_from(seconds.as_secs() / 86400).map_err(io::Error::other)?;
    // Gregorian civil date from Unix days (400-year eras account for leap years).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    let time = seconds.as_secs() % 86400;
    Ok(format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}{:03}Z",
        time / 3600,
        time / 60 % 60,
        time % 60,
        seconds.subsec_millis()
    ))
}

fn import_one(file: &str, agent: &Path, pi: &Path, suffix: &str) -> io::Result<LegacyPiImported> {
    if !LEGACY_PI_CONFIG_FILES.contains(&file) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not one of {}", LEGACY_PI_CONFIG_FILES.join(", ")),
        ));
    }
    let from = pi.join(file);
    let to = agent.join(file);
    if file_stat(&from)?.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", from.display()),
        ));
    }
    fs::create_dir_all(agent)?;
    let backup_path = if file_stat(&to)?.is_some() {
        let backup = agent.join(format!("{file}.bak-{suffix}"));
        fs::copy(&to, &backup)?;
        Some(backup)
    } else {
        None
    };
    fs::copy(&from, &to)?;
    Ok(LegacyPiImported {
        file: file.to_owned(),
        from,
        to,
        backup_path,
    })
}
pub fn import_legacy_pi_config(
    files: &[String],
    agent: &Path,
    home: &Path,
    now: SystemTime,
) -> io::Result<LegacyPiImportResult> {
    let pi = home.join(".pi/agent");
    let mut result = LegacyPiImportResult {
        pi_agent_dir: pi.clone(),
        agent_dir: agent.to_owned(),
        imported: Vec::new(),
        failures: Vec::new(),
    };
    if !separate(agent, &pi)? {
        result.failures.push(LegacyPiImportFailure {
            file: pi.display().to_string(),
            reason: "no separate pi config to import".to_owned(),
        });
        return Ok(result);
    }
    let requested = if files.is_empty() {
        find_legacy_pi_edits(agent, home)?
            .iter()
            .map(|edit| edit.file.to_owned())
            .collect()
    } else {
        files.to_vec()
    };
    let suffix = backup_suffix(now)?;
    for file in requested {
        match import_one(&file, agent, &pi, &suffix) {
            Ok(imported) => result.imported.push(imported),
            Err(error) => result.failures.push(LegacyPiImportFailure {
                file,
                reason: error.to_string(),
            }),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let pi = home.join(".pi/agent");
        let agent = home.join(".omo/agent");
        fs::create_dir_all(&pi).expect("pi");
        fs::create_dir_all(&agent).expect("agent");
        fs::write(pi.join("settings.json"), "changed").expect("pi settings");
        fs::write(agent.join("settings.json"), "original").expect("agent settings");
        fs::OpenOptions::new().write(true).open(pi.join("settings.json"))
            .expect("pi")
            .set_modified(UNIX_EPOCH + Duration::from_secs(200))
            .expect("mtime");
        fs::OpenOptions::new().write(true).open(agent.join("settings.json"))
            .expect("agent")
            .set_modified(UNIX_EPOCH + Duration::from_secs(100))
            .expect("mtime");
        (root, home, pi, agent)
    }

    #[test]
    fn notice_records_each_mtime_once_with_fallback_baseline() {
        // Given
        let (_root, home, _pi, agent) = fixture();
        let first = take_legacy_pi_edit_notice(&agent, &home).expect("first notice");
        // When
        let repeated = take_legacy_pi_edit_notice(&agent, &home).expect("repeated notice");
        // Then
        assert_eq!(
            first.iter().map(|edit| edit.file).collect::<Vec<_>>(),
            ["settings.json"]
        );
        assert!(repeated.is_empty());
    }

    #[test]
    fn copy_time_and_equal_contents_exclude_edits() {
        // Given
        let (_root, home, pi, agent) = fixture();
        fs::write(pi.join("auth.json"), "same").expect("pi auth");
        fs::write(agent.join("auth.json"), "same").expect("agent auth");
        fs::OpenOptions::new().write(true).open(pi.join("auth.json"))
            .expect("pi auth")
            .set_modified(UNIX_EPOCH + Duration::from_secs(400))
            .expect("auth mtime");
        crate::migrations_state::write_legacy_pi_agent_dir_record(
            &crate::migrations_state::LegacyPiAgentDirRecord {
                copied_at: Some(300000.0),
                ..Default::default()
            },
            &agent,
        )
        .expect("copy record");
        // When
        let edits = find_legacy_pi_edits(&agent, &home).expect("edits");
        // Then
        assert!(edits.is_empty());
    }

    #[test]
    fn import_defaults_to_changed_files_and_backs_up_destination() {
        // Given
        let (_root, home, pi, agent) = fixture();
        // When
        let result = import_legacy_pi_config(&[], &agent, &home, UNIX_EPOCH).expect("import");
        // Then
        assert!(result.failures.is_empty());
        assert_eq!(result.imported.len(), 1);
        assert_eq!(
            fs::read(agent.join("settings.json")).expect("imported"),
            b"changed"
        );
        assert_eq!(
            fs::read(agent.join("settings.json.bak-19700101T000000000Z")).expect("backup"),
            b"original"
        );
        assert_eq!(
            fs::read(pi.join("settings.json")).expect("source"),
            b"changed"
        );
    }

    #[test]
    fn explicit_import_reports_failures_without_stopping_other_files() {
        // Given
        let (_root, home, _pi, agent) = fixture();
        let files = ["../outside", "models.json", "settings.json"].map(str::to_owned);
        // When
        let result = import_legacy_pi_config(&files, &agent, &home, UNIX_EPOCH).expect("import");
        // Then
        assert_eq!(
            result
                .failures
                .iter()
                .map(|failure| failure.file.as_str())
                .collect::<Vec<_>>(),
            ["../outside", "models.json"]
        );
        assert_eq!(
            result
                .imported
                .iter()
                .map(|entry| entry.file.as_str())
                .collect::<Vec<_>>(),
            ["settings.json"]
        );
    }
}
