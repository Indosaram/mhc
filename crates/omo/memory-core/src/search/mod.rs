//! Query parsing, transcript providers, and ranked transcript search.

pub mod engine;
pub mod query;
pub mod senpi_session_provider;

pub use engine::{
    DEFAULT_LIMIT, SearchOptions, SearchResult, TranscriptConversation, TranscriptProvider,
    search_transcripts,
};
pub use query::{
    ParsedQuery, SearchDocument, SearchToolCall, date_in_range, match_score, normalize_text,
    parse_query, searchable_text,
};
pub use senpi_session_provider::{
    SenpiHiddenCandidate, SenpiSessionHeader, SenpiSessionProvider, SenpiSessionProviderOptions,
};
