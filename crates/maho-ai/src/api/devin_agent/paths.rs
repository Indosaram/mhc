//! Port of senpi packages/ai/src/api/devin-agent/paths.ts.
//!
//! Cascade RPC paths and wire constants.

/// `DEVIN_DEFAULT_BASE_URL`.
pub const DEVIN_DEFAULT_BASE_URL: &str = "https://server.codeium.com";
/// `DEVIN_CHAT_MESSAGE_PATH`.
pub const DEVIN_CHAT_MESSAGE_PATH: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
/// `DEVIN_CLI_MODEL_CONFIGS_PATH`.
pub const DEVIN_CLI_MODEL_CONFIGS_PATH: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";
/// `DEVIN_ASSIGN_MODEL_PATH`.
pub const DEVIN_ASSIGN_MODEL_PATH: &str = "/exa.api_server_pb.ApiServerService/AssignModel";
/// `DEVIN_USER_JWT_PATH`.
pub const DEVIN_USER_JWT_PATH: &str = "/exa.auth_pb.AuthService/GetUserJwt";

/// `DEVIN_CHAT_HEADERS`: the headers the released CLI sends on the streaming chat call. Auth rides
/// inside `Metadata.api_key`, so there is no `authorization` header.
pub const DEVIN_CHAT_HEADERS: [(&str, &str); 6] = [
    ("content-type", "application/connect+proto"),
    ("connect-protocol-version", "1"),
    ("connect-content-encoding", "gzip"),
    ("accept-encoding", "identity"),
    ("user-agent", "connect-go/1.18.1 (go1.26.3)"),
    ("connect-accept-encoding", "gzip"),
];

/// `DEVIN_UNARY_HEADERS`: unary Connect calls carry a bare protobuf body, not the 5-byte frame.
pub const DEVIN_UNARY_HEADERS: [(&str, &str); 3] =
    [("content-type", "application/proto"), ("connect-protocol-version", "1"), ("accept", "*/*")];

/// `DEVIN_COMPRESSED_FLAG`: bit 0 marks a gzipped payload.
pub const DEVIN_COMPRESSED_FLAG: u8 = 0x01;
/// `DEVIN_TRAILER_FLAG`: bit 1 marks the end-of-stream trailer.
pub const DEVIN_TRAILER_FLAG: u8 = 0x02;

/// `DEVIN_MAX_FRAME_PAYLOAD`: hard upper bound on one Connect frame payload. The 4-byte length
/// prefix can describe 4 GiB; a corrupted or hostile prefix must not turn into an allocation of
/// that size, so anything larger is a protocol error.
pub const DEVIN_MAX_FRAME_PAYLOAD: u32 = 64 * 1024 * 1024;
