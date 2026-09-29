//! Facts queue, failure management, extraction payload, and person routing.

pub mod assets;
pub mod extraction;
pub mod failures_backoff;
pub mod failures_schema;
pub mod failures_selection;
pub mod failures_store;
pub mod mutation_plan;
pub mod payload_cap;
pub mod person_index;
pub mod person_routing;
pub mod queue;
pub mod recovery;
pub mod recovery_mutation;
pub mod recovery_ownership;
pub mod schema;

pub use assets::{FACTS_PERSONA, load_facts_persona};
pub use extraction::{
    ApplyFactsBatchOptions, ApplyFactsBatchResult, FactsBatch, FactsExtractionError,
    FactsExtractionRecord, FactsExtractionValidationError, FactsPersonReference, apply_facts_batch,
    parse_facts_extraction_jsonl, validate_facts_recovery,
};
pub use failures_backoff::{
    ApplyFailureInput, FactsFailureFilter, FactsFailureTarget, apply_failure, clear_for_retry,
    clear_on_success,
};
pub use failures_schema::{
    FACTS_FAILURE_REASONS, FACTS_FAILURES_VERSION, FactsFailureReason, FactsFailureRecord,
    FactsFailureState, FactsFailuresCorruptError, FactsFailuresFile, empty_failures_file,
    parse_failures_file, render_failures_file, sort_failure_records,
};
pub use failures_selection::{
    FactsLaunchSelection, FactsSkipReason, facts_selection_key, select_launchable,
};
pub use payload_cap::{
    CappedFactsBatch, CappedFactsBatchInput, FACTS_STARVATION_MS, FactsKnownPerson, FactsPayload,
    FactsPayloadEnvelope, FactsPrimaryHuman, MAX_FACTS_PAYLOAD_BYTES, measure_facts_payload_bytes,
    select_capped_facts_batch, serialize_facts_payload,
};
pub use person_index::{FactsPeopleIndexEntry, display_name_of, read_facts_people_index};
pub use person_routing::{
    AliasTieCallback, FactsAliasTie, FactsPeopleRouting, FactsPersonTarget, FactsRoutingPlan,
    ObservationBucket, facts_routing_paths, normalize_observation_text, plan_facts_routing,
    render_card_skeleton, render_person_targets, resolve_person_slug,
};
pub use schema::{
    FACTS_QUEUE_VERSION, FactsConsumedRecord, FactsConsumedWatermark, FactsCursor, FactsQueueEntry,
    FactsQueueLayout, FactsQueueRange, canonical_position, facts_queue_paths, initial_cursor,
    parse_consumed, parse_cursor, parse_queue_entry, queue_timestamp,
};
