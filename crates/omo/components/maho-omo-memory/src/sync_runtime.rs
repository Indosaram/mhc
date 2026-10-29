//! Port of `memory/sync-runtime.ts`: proves the sync engine's file-backed assets load (the Loro
//! runtime and the case-fold table). In the Rust port both are linked in, so the check is a Loro
//! document round-trip plus the case-fold probe.

pub use memory_core::sync::engine::open_device_sync;

use loro::{ExportMode, LoroDoc};
use memory_core::sync::engine::case_fold;

/// Fails with the upstream message when the case-fold table is wrong, or with the Loro error text
/// when the document runtime cannot write, snapshot and re-read a text container.
pub fn check_sync_runtime() -> Result<(), String> {
    let doc = LoroDoc::new();
    doc.get_text("t")
        .insert(0, "hi")
        .map_err(|error| error.to_string())?;
    doc.commit();
    let snapshot = doc
        .export(ExportMode::Snapshot)
        .map_err(|error| error.to_string())?;
    let copy = LoroDoc::new();
    copy.import(&snapshot).map_err(|error| error.to_string())?;
    if copy.get_text("t").to_string() != "hi" {
        return Err("memory sync: the loro document did not round-trip".into());
    }
    if case_fold("Straße") != "strasse" {
        return Err("memory sync: the case-fold table did not load".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packaged_runtime_assets_load() {
        assert_eq!(check_sync_runtime(), Ok(()));
    }
}
