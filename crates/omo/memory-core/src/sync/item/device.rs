use super::{
    ItemError,
    device_peer::{load_or_create_device_peer, retire_aside},
    document::create_item_doc,
    machine_id::{device_fingerprint, read_machine_id},
};
use loro::LoroDoc;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct ItemDevice {
    pub peer: u64,
    pub state_dir: PathBuf,
}
pub struct OpenItemDeviceOptions<'a> {
    pub state_dir: &'a Path,
    pub memory_root: &'a Path,
    pub machine_id: Option<&'a str>,
}
pub fn open_item_device(options: OpenItemDeviceOptions<'_>) -> Result<ItemDevice, ItemError> {
    let state_dir = canonical(options.state_dir)?;
    let memory_root = canonical(options.memory_root)?;
    if state_dir.starts_with(&memory_root) {
        return Err(ItemError::DevicePeer(format!(
            "device state dir {} is inside the synced memory root {}",
            state_dir.display(),
            memory_root.display()
        )));
    }
    let machine = device_fingerprint(
        options.machine_id.unwrap_or_else(|| read_machine_id()),
        &state_dir.to_string_lossy(),
    );
    let device = load_or_create_device_peer(&state_dir.join("peer"), &machine)?;
    if device.retired {
        retire_aside(&state_dir.join("captures"))?;
    }
    Ok(ItemDevice {
        peer: device.peer,
        state_dir,
    })
}
pub fn create_device_item_doc(device: &ItemDevice) -> Result<LoroDoc, ItemError> {
    create_item_doc(device.peer)
}
pub fn assert_device_doc(device: &ItemDevice, doc: &LoroDoc) -> Result<(), ItemError> {
    if device.peer != doc.peer_id() {
        Err(ItemError::DevicePeer(format!(
            "item doc peer {} is not the device peer {}",
            doc.peer_id(),
            device.peer
        )))
    } else {
        Ok(())
    }
}
fn canonical(path: &Path) -> Result<PathBuf, ItemError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => (),
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(c),
        }
    }
    let mut probe = normalized.clone();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(&probe) {
            Ok(mut found) => {
                for segment in missing.iter().rev() {
                    found.push(segment);
                }
                return Ok(found);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name() else {
                    return Ok(normalized);
                };
                missing.push(name.to_os_string());
                if !probe.pop() {
                    return Ok(normalized);
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}
