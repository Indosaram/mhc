//! Port of senpi packages/ai/src/api/devin-agent/wire.ts.
//!
//! Cascade wire surface: paths, identity metadata, frame codec, unary RPCs and request building.

pub use crate::api::devin_agent::frames::{decode_devin_frames, encode_devin_frame as encode_devin_request_frame, DevinFrame};
pub use crate::api::devin_agent::metadata::{
    devin_cli_metadata, devin_discovery_metadata, normalize_devin_session_token, DEVIN_CLI_IDENTITY,
    DEVIN_DISCOVERY_IDENTITY, DEVIN_SUPPORTED_MODEL_DISPLAYS,
};
pub use crate::api::devin_agent::paths::{
    DEVIN_ASSIGN_MODEL_PATH, DEVIN_CHAT_HEADERS, DEVIN_CHAT_MESSAGE_PATH, DEVIN_CLI_MODEL_CONFIGS_PATH,
    DEVIN_DEFAULT_BASE_URL, DEVIN_MAX_FRAME_PAYLOAD, DEVIN_UNARY_HEADERS, DEVIN_USER_JWT_PATH,
};
pub use crate::api::devin_agent::request::{
    build_devin_chat_request, build_devin_router_prompt, DevinChatRequestInput, DevinModelAssignment,
    DEVIN_DEFAULT_STOP_PATTERNS,
};
pub use crate::api::devin_agent::trailer::{read_devin_trailer_error, DevinTrailerError};
pub use crate::api::devin_agent::unary::{decode_devin_unary, post_devin_unary, DevinUnaryError, DevinUnaryInput};
