//! Port of omo-senpi `components/x-search` at latest `455dee62`.
//!
//! One upstream source file per Rust module, names and order preserved:
//! - `auth.ts` -> [`auth`]
//! - `client.ts` -> [`client`]
//! - `format.ts` -> [`format`]
//! - `params.ts` -> [`params`]
//! - `tool.ts` -> [`tool`]
//! - `index.ts` -> [`index`]

pub mod auth;
pub mod client;
pub mod format;
pub mod index;
pub mod params;
pub mod tool;

pub use auth::{XAI_PROVIDER, has_xai_credential, resolve_xai_bearer};
pub use client::{
    DEFAULT_CARRIER, DEFAULT_PROMPT_VARIANT, PerformXSearchOptions, ReasoningEffort,
    X_SEARCH_DEFAULT_DEADLINE_MS, X_SEARCH_ENDPOINT, X_SEARCH_MAX_OUTPUT_TOKENS, XSearchCarrier,
    XSearchCarrierName, XSearchFetch, XSearchFetchOutcome, XSearchHttpResponse,
    XSearchInputMessage, XSearchPromptVariant, XSearchRequestBody, XSearchRequestParams,
    build_x_search_request, carrier_model, default_fetch, normalize_x_search_response,
    perform_x_search, prompt_v1, prompt_v2, render_prompt,
};
pub use format::{
    EMPTY_RESULT_TEXT, NormalizedXSearch, XSearchErrorCode, XSearchResultItem, XSearchUsage,
    format_x_search_error, format_x_search_result,
};
pub use index::{
    SkillPathResolver, X_SEARCH_COMPONENT_NAME, XSearchComponent, XSearchComponentOptions,
    create_x_search_component, resolve_x_search_skill_path,
};
pub use params::{
    XSearchParams, XSearchValidation, validate_x_search_params, x_search_parameters_schema,
};
pub use tool::{
    BearerResolver, CreateXSearchToolOptions, X_SEARCH_MODEL_ENV, X_SEARCH_TOOL_DESCRIPTION,
    X_SEARCH_TOOL_NAME, XSearchToolDetails, create_x_search_tool,
};
