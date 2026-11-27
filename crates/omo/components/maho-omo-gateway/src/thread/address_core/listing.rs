//! Device listing round status (`packages/thread-address-core/src/listing.ts`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// How a listing round saw a device: answered in full, did not answer in full, or offline in the roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceListing {
    Listed,
    Incomplete,
    Offline,
}

impl DeviceListing {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Incomplete => "incomplete",
            Self::Offline => "offline",
        }
    }
}

/// One listing round: the devices it names incomplete, and each device's own round state when the service reports one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingRound {
    pub incomplete: Vec<String>,
    pub states: HashMap<String, String>,
}

/// The one listing state for a device, the same for the list and the resolver (design section 6).
pub fn device_listing(
    device: &str,
    round: &ListingRound,
    roster: &[(String, bool)],
) -> DeviceListing {
    if roster.iter().any(|(id, online)| id == device && !*online) {
        return DeviceListing::Offline;
    }
    let state = round.states.get(device).map(String::as_str);
    if round.incomplete.iter().any(|d| d == device) || matches!(state, Some(s) if s != "answered") {
        DeviceListing::Incomplete
    } else {
        DeviceListing::Listed
    }
}
