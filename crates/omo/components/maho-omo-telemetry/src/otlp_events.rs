//! Port of `packages/omo-senpi/src/components/telemetry/otlp-events.ts`.
//!
//! Event names from collector pin 13c6e61b40c4, kept outside the exporter projection so a legacy-only
//! component can reject its event without building the exporter.

pub const OTLP_EVENTS: [&str; 17] = [
    "category_config",
    "computer_use_activation",
    "computer_use_engine_error",
    "computer_use_permission_denied",
    "daily_active",
    "delegation_completed",
    "delegation_started",
    "feature_used",
    "handoff_hint",
    "kibitzer_summary",
    "parallelism_summary",
    "process_crashed",
    "prompt_submitted",
    "session_started",
    "session_summary",
    "skill_loaded",
    "turn_completed",
];

pub fn is_otlp_event(event: &str) -> bool {
    OTLP_EVENTS.contains(&event)
}
