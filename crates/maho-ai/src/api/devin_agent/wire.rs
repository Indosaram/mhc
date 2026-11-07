//! Port of senpi packages/ai/src/api/devin-agent/wire.ts.
//!
//! Cascade wire surface: paths, identity metadata, frame codec, unary RPCs and request building.

pub use crate::api::devin_agent::frames::{
    DevinFrame, decode_devin_frames, encode_devin_frame as encode_devin_request_frame,
};
pub use crate::api::devin_agent::metadata::{
    DEVIN_CLI_IDENTITY, DEVIN_DISCOVERY_IDENTITY, DEVIN_SUPPORTED_MODEL_DISPLAYS,
    devin_cli_metadata, devin_discovery_metadata, normalize_devin_session_token,
};
pub use crate::api::devin_agent::paths::{
    DEVIN_ASSIGN_MODEL_PATH, DEVIN_CHAT_HEADERS, DEVIN_CHAT_MESSAGE_PATH,
    DEVIN_CLI_MODEL_CONFIGS_PATH, DEVIN_DEFAULT_BASE_URL, DEVIN_MAX_FRAME_PAYLOAD,
    DEVIN_UNARY_HEADERS, DEVIN_USER_JWT_PATH,
};
pub use crate::api::devin_agent::request::{
    DEVIN_DEFAULT_STOP_PATTERNS, DevinChatRequestInput, DevinModelAssignment,
    build_devin_chat_request, build_devin_router_prompt,
};
pub use crate::api::devin_agent::trailer::{DevinTrailerError, read_devin_trailer_error};
pub use crate::api::devin_agent::unary::{
    DevinUnaryError, DevinUnaryInput, decode_devin_unary, post_devin_unary,
};
