//! Root module for `maho-omo-post-mutation`.

pub mod post_mutation;

pub use post_mutation::{
    create_post_mutation_session_state, extract_mutated_file_paths, is_mutation_tool_name,
    run_single_flight, PostMutationEvent, PostMutationSessionState, SingleFlight,
    MUTATION_TOOL_NAMES,
};
