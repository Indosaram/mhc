//! Per-device CRDT memory items.
#![deny(clippy::unwrap_used)]
pub mod adopt_values;
pub mod capture;
pub mod capture_receipts;
pub mod casefold;
pub mod device;
pub mod device_capture;
pub mod device_peer;
pub mod document;
pub mod field_delta;
pub mod frontmatter;
pub mod history;
pub mod key;
pub mod key_writers;
pub mod machine_id;
pub mod render;
pub mod render_for_disk;
pub mod state;
pub mod tag;
pub mod three_way;
pub mod transplant;
pub mod yaml;

pub use adopt_values::{RegisterValue, active_registers};
pub use casefold::case_fold;
pub use device::{ItemDevice, OpenItemDeviceOptions, create_device_item_doc, open_item_device};
pub use device_capture::{capture_on_device, record_materialized};
pub use document::create_item_doc;
pub use document::{ItemFrontier, delete_item, is_live, item_frontier};
pub use frontmatter::{FmEntry, FmValue, ParsedItemText, split_item};
pub use key::normalize_item_key;
pub use render::{render_doc_content, render_item};
pub use render_for_disk::render_for_disk;

#[derive(Debug)]
pub enum ItemError {
    Yaml(String),
    Crdt(String),
    CaptureBaseline,
    DevicePeer(String),
    Receipt(String),
    Io(std::io::Error),
}
impl std::fmt::Display for ItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yaml(s) | Self::Crdt(s) | Self::DevicePeer(s) | Self::Receipt(s) => {
                f.write_str(s)
            }
            Self::CaptureBaseline => {
                f.write_str("captureItem: baseText is not the item's render at baseFrontier")
            }
            Self::Io(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for ItemError {}
impl From<std::io::Error> for ItemError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
pub(crate) fn crdt(e: impl std::fmt::Display) -> ItemError {
    ItemError::Crdt(e.to_string())
}

#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod device_tests;
#[cfg(test)]
mod property_model;
#[cfg(test)]
mod property_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;
