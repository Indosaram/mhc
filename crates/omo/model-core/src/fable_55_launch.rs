//! Launch gate for the provisional Claude Fable 5.5 id (port of `fable-55-launch.ts`).

use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::PoisonError;

use crate::latest_models::LATEST_MODEL_BY_FAMILY;
use crate::model_capabilities::ModelCapabilitiesSnapshotEntry;
use crate::model_capabilities::SnapshotLimit;
use crate::model_requirement_types::FallbackEntry;

/// The provisional launch id (`FABLE_55_LAUNCH.id`).
pub const FABLE_55_LAUNCH_ID: &str = "claude-fable-5-5";

// LAUNCH-GATED: the ID is provisional. Fill only from the live launch listing, never from 5.1.
// Pricing is owned by the adopted senpi catalog; omo has no price override for this model.
pub static FABLE_55_LAUNCH: LazyLock<ModelCapabilitiesSnapshotEntry> =
    LazyLock::new(|| ModelCapabilitiesSnapshotEntry {
        id: FABLE_55_LAUNCH_ID.to_string(),
        family: Some("claude-fable".to_string()),
        reasoning: None,
        temperature: None,
        tool_call: None,
        modalities: None,
        limit: Some(SnapshotLimit {
            context: None,
            input: None,
            output: None,
        }),
    });

// Flip to true only after the live listing confirms the id. Until then no routing path may pick it
// without availability proof (a cold provider-models cache gives none).
pub const FABLE_55_RELEASED: bool = false;

/// Effort table for the 5.5 rungs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fable55Efforts {
    pub measured: bool,
    pub accepted: &'static [&'static str],
}

// LAUNCH-GATED: the 5.5 rungs carry xhigh/max carried over from 5.1, not measured on 5.5. Set
// measured to true and accepted to the efforts the live listing confirms before the release flip.
pub const FABLE_55_EFFORTS: Fable55Efforts = Fable55Efforts {
    measured: false,
    accepted: &["xhigh", "max"],
};

fn is_fable55_model(model: &str) -> bool {
    let model_id = model.rsplit('/').next().unwrap_or(model);
    model_id.replace('.', "-") == FABLE_55_LAUNCH_ID
}

#[must_use]
pub fn is_launch_gated_model(model: &str) -> bool {
    if FABLE_55_RELEASED {
        return false;
    }
    is_fable55_model(model)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRungRejection {
    pub model: String,
    pub variant: Option<String>,
}

static LAUNCH_RUNG_REJECTION_LOG: Mutex<Vec<LaunchRungRejection>> = Mutex::new(Vec::new());

/// Rejections recorded while building the builtin chains; a release build must record none.
#[must_use]
pub fn launch_rung_rejections() -> Vec<LaunchRungRejection> {
    LAUNCH_RUNG_REJECTION_LOG
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Release gate passed to [`select_launch_rungs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchGate<'a> {
    pub released: bool,
    pub accepted: &'a [&'a str],
}

// A launch-gate config error must never take down plugin load: a released 5.5 rung whose effort is
// not accepted is dropped (treated as unreleased), recorded, and warned about once; the launch gate
// test fails on any recorded rejection, so the error is caught before it ships.
pub fn select_launch_rungs(
    rungs: Vec<FallbackEntry>,
    gate: LaunchGate<'_>,
    mut on_rejected: impl FnMut(LaunchRungRejection),
) -> Vec<FallbackEntry> {
    rungs
        .into_iter()
        .filter(|rung| {
            if !is_fable55_model(&rung.model) {
                return true;
            }
            if !gate.released {
                return false;
            }
            if rung
                .variant
                .as_deref()
                .is_some_and(|variant| gate.accepted.contains(&variant))
            {
                return true;
            }
            on_rejected(LaunchRungRejection {
                model: rung.model.clone(),
                variant: rung.variant.clone(),
            });
            false
        })
        .collect()
}

fn record_launch_rung_rejection(rejection: LaunchRungRejection) {
    eprintln!(
        "[fable-55-launch] dropped a Fable 5.5 rung with effort \"{}\": not in FABLE_55_EFFORTS.accepted",
        rejection.variant.as_deref().unwrap_or("(none)")
    );
    LAUNCH_RUNG_REJECTION_LOG
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(rejection);
}

/// The model that stands in for Fable 5.5 until it is released (its launch-day predecessor): the
/// table entry, which stays on 5.1 until after launch. Moving `claudeFable` to 5.5 is the
/// POST-launch cleanup step, after `FABLE_55_RELEASED` flips.
#[must_use]
pub fn fable_55_predecessor() -> &'static str {
    LATEST_MODEL_BY_FAMILY
        .iter()
        .find(|(family, _)| *family == "claudeFable")
        .map_or("claude-fable-5-1", |(_, model)| *model)
}

// Data-layer gate: every builtin chain, profile and default builds its 5.5 entries through these, so
// while FABLE_55_RELEASED is false no resolver can ever see the provisional id.
#[must_use]
pub fn launch_rungs(rungs: Vec<FallbackEntry>) -> Vec<FallbackEntry> {
    select_launch_rungs(
        rungs,
        LaunchGate {
            released: FABLE_55_RELEASED,
            accepted: FABLE_55_EFFORTS.accepted,
        },
        record_launch_rung_rejection,
    )
}

#[must_use]
pub fn launch_default_model_id() -> &'static str {
    if FABLE_55_RELEASED {
        FABLE_55_LAUNCH_ID
    } else {
        fable_55_predecessor()
    }
}
