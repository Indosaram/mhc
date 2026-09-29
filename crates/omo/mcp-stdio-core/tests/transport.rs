use std::io::Write;

use mcp_stdio_core::JsonRpcId;
use mcp_stdio_core::JsonRpcResponse;
use mcp_stdio_core::JsonRpcResult;
use mcp_stdio_core::StdioJsonRpcDecoder;
use mcp_stdio_core::StdioJsonRpcMessage;
use mcp_stdio_core::StdioJsonRpcResponseMode;
use mcp_stdio_core::decode_stdio_json_rpc_messages;
use mcp_stdio_core::success_response;
use mcp_stdio_core::write_stdio_json_rpc_response;
use serde_json::json;

fn empty_result() -> JsonRpcResult {
    JsonRpcResult::new()
}

fn render(response: &JsonRpcResponse, mode: StdioJsonRpcResponseMode) -> String {
    let mut output = Vec::new();
    write_stdio_json_rpc_response(&mut output, response, mode).expect("write");
    String::from_utf8(output).expect("utf8")
}

#[test]
fn line_delimited_json_yields_line_mode_requests() {
    let messages =
        decode_stdio_json_rpc_messages(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
    assert_eq!(
        messages,
        vec![StdioJsonRpcMessage::Request {
            payload: json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
            response_mode: StdioJsonRpcResponseMode::Line,
        }]
    );
}

#[test]
fn content_length_json_yields_framed_requests() {
    let body = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\"}";
    let input = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    let messages = decode_stdio_json_rpc_messages(input.as_bytes());
    assert_eq!(
        messages,
        vec![StdioJsonRpcMessage::Request {
            payload: json!({"jsonrpc": "2.0", "id": 2, "method": "initialize"}),
            response_mode: StdioJsonRpcResponseMode::Framed,
        }]
    );
}

#[test]
fn response_mode_controls_the_framing_bytes() {
    let response = success_response(JsonRpcId::Number(1.into()), empty_result());
    assert_eq!(
        render(&response, StdioJsonRpcResponseMode::Framed),
        "Content-Length: 36\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"
    );
    assert_eq!(
        render(&response, StdioJsonRpcResponseMode::Line),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n"
    );
}

#[test]
fn framed_content_length_counts_utf8_bytes_not_characters() {
    let mut result = empty_result();
    result.insert("text".to_string(), json!("한글"));
    let response = success_response(JsonRpcId::Null, result);
    let rendered = render(&response, StdioJsonRpcResponseMode::Framed);
    let (headers, body) = rendered.split_once("\r\n\r\n").expect("separator");
    let declared: usize = headers
        .strip_prefix("Content-Length: ")
        .expect("header")
        .parse()
        .expect("number");
    assert_eq!(declared, body.len());
}

#[test]
fn a_partial_frame_is_buffered_until_it_completes() {
    let body = "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}";
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    let (head, tail) = frame.split_at(20);

    let mut decoder = StdioJsonRpcDecoder::new();
    assert_eq!(decoder.push(head.as_bytes()), Vec::new());
    assert_eq!(
        decoder.push(tail.as_bytes()),
        vec![StdioJsonRpcMessage::Request {
            payload: json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}),
            response_mode: StdioJsonRpcResponseMode::Framed,
        }]
    );
}

#[test]
fn several_framed_messages_in_one_chunk_are_all_yielded() {
    let first = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"a\"}";
    let second = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"b\"}";
    let input = format!(
        "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
        first.len(),
        first,
        second.len(),
        second
    );
    let messages = decode_stdio_json_rpc_messages(input.as_bytes());
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].response_mode(),
        StdioJsonRpcResponseMode::Framed
    );
    assert_eq!(
        messages[1].response_mode(),
        StdioJsonRpcResponseMode::Framed
    );
}

#[test]
fn blank_lines_are_skipped() {
    let messages = decode_stdio_json_rpc_messages(b"\n\r\n   \n{\"id\":9}\n");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].response_mode(), StdioJsonRpcResponseMode::Line);
}

#[test]
fn a_carriage_return_before_the_newline_is_stripped() {
    let messages = decode_stdio_json_rpc_messages(b"{\"id\":4}\r\n");
    assert_eq!(
        messages,
        vec![StdioJsonRpcMessage::Request {
            payload: json!({"id": 4}),
            response_mode: StdioJsonRpcResponseMode::Line,
        }]
    );
}

#[test]
fn a_trailing_unterminated_payload_is_flushed_as_a_line_message() {
    let mut decoder = StdioJsonRpcDecoder::new();
    assert_eq!(decoder.push(b"{\"id\":5,\"method\":\"tail\"}"), Vec::new());
    assert_eq!(
        decoder.finish(),
        Some(StdioJsonRpcMessage::Request {
            payload: json!({"id": 5, "method": "tail"}),
            response_mode: StdioJsonRpcResponseMode::Line,
        })
    );
}

#[test]
fn whitespace_only_trailing_bytes_produce_no_message() {
    let mut decoder = StdioJsonRpcDecoder::new();
    decoder.push(b"   \r\n");
    assert_eq!(decoder.finish(), None);
}

#[test]
fn malformed_json_reports_a_line_mode_parse_error() {
    let messages = decode_stdio_json_rpc_messages(b"garbage\n");
    match &messages[0] {
        StdioJsonRpcMessage::ParseError {
            message,
            response_mode,
        } => {
            assert!(!message.is_empty());
            assert_eq!(*response_mode, StdioJsonRpcResponseMode::Line);
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn malformed_framed_body_reports_a_framed_parse_error() {
    let body = "not json";
    let input = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    let messages = decode_stdio_json_rpc_messages(input.as_bytes());
    match &messages[0] {
        StdioJsonRpcMessage::ParseError {
            message,
            response_mode,
        } => {
            assert!(!message.is_empty());
            assert_eq!(*response_mode, StdioJsonRpcResponseMode::Framed);
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn a_missing_content_length_header_is_its_own_parse_error() {
    let messages = decode_stdio_json_rpc_messages(b"Content-Length: not-a-number\r\n\r\n{}");
    assert_eq!(
        messages,
        vec![
            StdioJsonRpcMessage::ParseError {
                message: "Missing or invalid Content-Length header".to_string(),
                response_mode: StdioJsonRpcResponseMode::Framed,
            },
            StdioJsonRpcMessage::Request {
                payload: json!({}),
                response_mode: StdioJsonRpcResponseMode::Line,
            },
        ]
    );
}

#[test]
fn the_content_length_prefix_match_is_case_insensitive() {
    let body = "{\"id\":1}";
    let input = format!("content-length: {}\r\n\r\n{}", body.len(), body);
    let messages = decode_stdio_json_rpc_messages(input.as_bytes());
    assert_eq!(
        messages[0].response_mode(),
        StdioJsonRpcResponseMode::Framed
    );
}

#[test]
fn a_failing_writer_surfaces_the_write_error() {
    struct AlwaysFails;

    impl Write for AlwaysFails {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "parent output closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let response = success_response(JsonRpcId::Number(1.into()), empty_result());
    let error =
        write_stdio_json_rpc_response(&mut AlwaysFails, &response, StdioJsonRpcResponseMode::Line)
            .expect_err("a closed writer must surface its error");
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
fn a_serialization_failure_is_reported_as_an_error() {
    struct Unserializable;

    impl serde::Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("synthetic serialization failure"))
        }
    }

    let mut output = Vec::new();
    let error =
        write_stdio_json_rpc_response(&mut output, &Unserializable, StdioJsonRpcResponseMode::Line)
            .expect_err("a non-serializable response must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
