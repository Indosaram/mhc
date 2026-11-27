//! Port of `packages/coding-agent/src/engine-trust/hand-off.ts` (senpi#6312).

use std::collections::BTreeMap;

use super::adopted::adopted_engine_trust;
use super::launch_environment::{TrustLaunchOptions, build_trust_launch_environment};
use super::marker::{EngineTrustMarker, EngineTrustRestore};

pub fn engine_trust_hand_off_environment(
    marker: &EngineTrustMarker,
    platform: &str,
) -> BTreeMap<String, String> {
    let cleared = EngineTrustMarker {
        restore: EngineTrustRestore::default(),
        ..marker.clone()
    };
    build_trust_launch_environment(
        &BTreeMap::new(),
        &TrustLaunchOptions {
            marker: &cleared,
            hop: false,
            platform,
        },
    )
    .unwrap_or_default()
}

pub fn adopted_engine_trust_hand_off(platform: Option<&str>) -> BTreeMap<String, String> {
    let platform = platform.unwrap_or(if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    });
    let Some(marker) = adopted_engine_trust() else {
        return BTreeMap::new();
    };
    engine_trust_hand_off_environment(&marker, platform)
}
