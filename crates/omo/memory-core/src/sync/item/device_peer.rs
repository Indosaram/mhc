use super::ItemError;
use crate::support::random::random_id;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
#[derive(Debug)]
pub struct DevicePeer {
    pub peer: u64,
    pub retired: bool,
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ItemError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}
fn read_optional(path: &Path) -> Result<Option<String>, ItemError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn read_peer(path: &Path) -> Result<Option<(u64, Option<String>)>, ItemError> {
    let Some(raw) = read_optional(&path.join("id"))? else {
        return Ok(None);
    };
    let text = raw.trim();
    let peer = text
        .parse::<u64>()
        .ok()
        .filter(|p| *p != 0 && p.to_string() == text)
        .ok_or_else(|| {
            ItemError::DevicePeer(format!(
                "device peer file is not a u64 peer id: {}",
                path.display()
            ))
        })?;
    let machine = read_optional(&path.join("machine"))?.map(|s| s.trim().into());
    Ok(Some((peer, machine)))
}
pub fn retire_aside(path: &Path) -> Result<(), ItemError> {
    let target = path.with_file_name(format!(
        "{}.retired-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        random_id()
    ));
    match fs::rename(path, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub fn load_or_create_device_peer(path: &Path, machine: &str) -> Result<DevicePeer, ItemError> {
    let existing = read_peer(path)?;
    if let Some((peer, Some(old))) = &existing
        && old == machine
    {
        return Ok(DevicePeer {
            peer: *peer,
            retired: false,
        });
    }
    if existing.is_some() {
        retire_aside(path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let candidate = path.with_file_name(format!("peer.{}.{}.tmp", std::process::id(), random_id()));
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&candidate)?;
    let publish = (|| {
        let mut peer = 0;
        while peer == 0 {
            peer = loro::LoroDoc::new().peer_id();
        }
        write_private(&candidate.join("id"), format!("{peer}\n").as_bytes())?;
        write_private(
            &candidate.join("machine"),
            format!("{machine}\n").as_bytes(),
        )?;
        fs::rename(&candidate, path)?;
        Ok::<(), ItemError>(())
    })();
    match fs::remove_dir_all(&candidate) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    if let Some((peer, Some(found))) = read_peer(path)?
        && found == machine
    {
        return Ok(DevicePeer {
            peer,
            retired: existing.is_some(),
        });
    }
    publish?;
    Err(ItemError::DevicePeer(format!(
        "device peer could not be created for this machine: {}",
        path.display()
    )))
}
