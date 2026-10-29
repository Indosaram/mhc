use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::clock::{now_ms, system_time_ms};
use crate::config::TeamModeConfig;
use crate::error::Result;
use crate::team_registry::paths::{get_inbox_dir, resolve_base_dir};

use super::ack::create_private_dir_all;

const RESERVED_PREFIX: &str = ".delivering-";
const RESERVED_SUFFIX: &str = ".json";

/// `DeliveryReservation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReservation {
    pub reserved_path: PathBuf,
    pub inbox_path: PathBuf,
    pub processed_path: PathBuf,
    pub processed_dir: PathBuf,
}

fn build_reservation(inbox_dir: &Path, message_id: &str) -> DeliveryReservation {
    let processed_dir = inbox_dir.join("processed");
    DeliveryReservation {
        reserved_path: inbox_dir.join(format!("{RESERVED_PREFIX}{message_id}{RESERVED_SUFFIX}")),
        inbox_path: inbox_dir.join(format!("{message_id}.json")),
        processed_path: processed_dir.join(format!("{message_id}.json")),
        processed_dir,
    }
}

/// `reserveMessageForDelivery`: confirm a pre-reserved file or rename the unread one into the slot.
pub fn reserve_message_for_delivery(
    team_run_id: &str,
    recipient_name: &str,
    message_id: &str,
    config: &TeamModeConfig,
) -> Result<Option<DeliveryReservation>> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, recipient_name)?;
    let reservation = build_reservation(&inbox_dir, message_id);
    match fs::metadata(&reservation.reserved_path) {
        Ok(_) => return Ok(Some(reservation)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::rename(&reservation.inbox_path, &reservation.reserved_path) {
        Ok(()) => Ok(Some(reservation)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// `commitDeliveryReservation`.
pub fn commit_delivery_reservation(reservation: &DeliveryReservation) -> Result<()> {
    create_private_dir_all(&reservation.processed_dir)?;
    fs::rename(&reservation.reserved_path, &reservation.processed_path)?;
    Ok(())
}

/// `releaseDeliveryReservation`.
pub fn release_delivery_reservation(reservation: &DeliveryReservation) -> Result<()> {
    fs::rename(&reservation.reserved_path, &reservation.inbox_path)?;
    Ok(())
}

/// `reclaimStaleReservations`: return reservations older than `stale_ttl_ms` to the inbox.
pub fn reclaim_stale_reservations(
    team_run_id: &str,
    recipient_name: &str,
    config: &TeamModeConfig,
    stale_ttl_ms: i64,
) -> Result<Vec<String>> {
    let inbox_dir = get_inbox_dir(&resolve_base_dir(config), team_run_id, recipient_name)?;
    let cutoff = now_ms() - stale_ttl_ms;
    let entries = match fs::read_dir(&inbox_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut reclaimed = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(message_id) = name
            .strip_prefix(RESERVED_PREFIX)
            .and_then(|rest| rest.strip_suffix(RESERVED_SUFFIX))
        else {
            continue;
        };
        let file_path = inbox_dir.join(&name);
        if system_time_ms(fs::metadata(&file_path)?.modified()?) > cutoff {
            continue;
        }
        if fs::rename(&file_path, inbox_dir.join(format!("{message_id}.json"))).is_ok() {
            reclaimed.push(message_id.to_owned());
        }
    }
    Ok(reclaimed)
}
