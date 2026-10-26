//! Embedded prelude and doc assets mirrored from `src/assets.ts`.

use std::sync::OnceLock;
use serde::{Deserialize, Serialize};

/// The five prelude and doc assets, generated into `assets.generated.json` by
/// `scripts/generate-assets.ts`; field names are the historical generated-constant names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreludeAssets {
    #[serde(rename = "COMPUTER_PRELUDE_JAVASCRIPT")]
    pub computer_prelude_javascript: String,
    #[serde(rename = "COMPUTER_PRELUDE_PYTHON")]
    pub computer_prelude_python: String,
    #[serde(rename = "COMPUTER_DECLARATIONS")]
    pub computer_declarations: String,
    #[serde(rename = "COMPUTER_DOCUMENTATION")]
    pub computer_documentation: String,
    #[serde(rename = "COMPUTER_SAFETY")]
    pub computer_safety: String,
}

pub const ASSETS_JSON_RAW: &str = include_str!("assets.generated.json");

static CACHED_ASSETS: OnceLock<PreludeAssets> = OnceLock::new();

/// Reads the embedded generated asset JSON once per process.
pub fn prelude_assets() -> &'static PreludeAssets {
    CACHED_ASSETS.get_or_init(|| {
        serde_json::from_str(ASSETS_JSON_RAW)
            .expect("senpi-desktop-prelude asset JSON is malformed embedded assets.generated.json")
    })
}
