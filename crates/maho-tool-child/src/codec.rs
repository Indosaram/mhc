//! Length-prefixed binary frames for the tool-child channel (`codec.ts`). A frame is a 4-byte big-endian body length
//! followed by that many bytes of UTF-8 JSON. No delimiter, no NDJSON.

use std::fmt;

use serde_json::Value;

use crate::protocol::{TOOL_CHILD_LIMITS, ToolChildMessage};
use crate::protocol_guard::is_tool_child_message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChildCodecErrorCode {
    ToolChildFrameTooLarge,
    ToolChildTruncated,
    ToolChildMalformed,
    ToolChildEncodeInvalid,
}

impl ToolChildCodecErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolChildFrameTooLarge => "tool_child_frame_too_large",
            Self::ToolChildTruncated => "tool_child_truncated",
            Self::ToolChildMalformed => "tool_child_malformed",
            Self::ToolChildEncodeInvalid => "tool_child_encode_invalid",
        }
    }
}

/// `ToolChildCodecError`: `message` is `"<code>: <detail>"`, like upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolChildCodecError {
    pub code: ToolChildCodecErrorCode,
    pub message: String,
}

impl ToolChildCodecError {
    pub fn new(code: ToolChildCodecErrorCode, message: &str) -> Self {
        Self {
            code,
            message: format!("{}: {message}", code.as_str()),
        }
    }
}

impl fmt::Display for ToolChildCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolChildCodecError {}

/// A frame is a 4-byte big-endian body length followed by that many bytes of UTF-8 JSON. No delimiter, no NDJSON.
pub const FRAME_HEADER_BYTES: usize = 4;

fn too_large(bytes: usize) -> ToolChildCodecError {
    ToolChildCodecError::new(
        ToolChildCodecErrorCode::ToolChildFrameTooLarge,
        &format!("{bytes} bytes exceeds {}", TOOL_CHILD_LIMITS.frame_bytes),
    )
}

pub fn encode_frame(message: &ToolChildMessage) -> Result<Vec<u8>, ToolChildCodecError> {
    // Optional fields that are `None` are skipped by serde (stringify's default drop for `undefined`); a required
    // json field is a `Value`, so it can never be `undefined` and NaN/Infinity are already `null`. The round-trip
    // with the decoder's own predicate refuses anything else unsendable HERE - a typed per-message error, never a
    // channel-killing frame (senpi#3569 delta review).
    let invalid = || {
        ToolChildCodecError::new(
            ToolChildCodecErrorCode::ToolChildEncodeInvalid,
            "the message serializes to a frame the peer's guard rejects",
        )
    };
    let json = serde_json::to_vec(message).map_err(|_| invalid())?;
    let body_bytes = json.len();
    if body_bytes > TOOL_CHILD_LIMITS.frame_bytes {
        return Err(too_large(body_bytes));
    }
    let reparsed: Value = serde_json::from_slice(&json).map_err(|_| invalid())?;
    if !is_tool_child_message(&reparsed) {
        return Err(invalid());
    }
    Ok(frame_from_body(&json))
}

fn frame_from_body(json: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + json.len());
    frame.extend_from_slice(&(json.len() as u32).to_be_bytes());
    frame.extend_from_slice(json);
    frame
}

fn parse_body(body: &[u8]) -> Result<ToolChildMessage, ToolChildCodecError> {
    let malformed = |detail: &str| {
        ToolChildCodecError::new(ToolChildCodecErrorCode::ToolChildMalformed, detail)
    };
    let value: Value =
        serde_json::from_slice(body).map_err(|_| malformed("frame body is not JSON"))?;
    if !is_tool_child_message(&value) {
        return Err(malformed("frame is not a protocol message"));
    }
    ToolChildMessage::from_value(value).map_err(|_| malformed("frame is not a protocol message"))
}

/// Incremental decoder for one channel direction. A length header above the cap is refused as soon as its four bytes
/// arrive, before any body buffer exists. Any codec error is terminal: the stream is never resynchronized, so nothing
/// after a bad frame can be silently skipped.
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    body_bytes: Option<usize>,
    failure: Option<ToolChildCodecError>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ToolChildMessage>, ToolChildCodecError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        match self.drain(&mut messages) {
            Ok(()) => Ok(messages),
            Err(error) => {
                self.fail(error.clone());
                Err(error)
            }
        }
    }

    fn drain(&mut self, messages: &mut Vec<ToolChildMessage>) -> Result<(), ToolChildCodecError> {
        loop {
            let body_bytes = match self.body_bytes {
                Some(bytes) => bytes,
                None => {
                    let Some(header) = self.buffer.first_chunk::<FRAME_HEADER_BYTES>() else {
                        return Ok(());
                    };
                    let length = u32::from_be_bytes(*header) as usize;
                    if length > TOOL_CHILD_LIMITS.frame_bytes {
                        return Err(too_large(length));
                    }
                    self.buffer.drain(..FRAME_HEADER_BYTES);
                    self.body_bytes = Some(length);
                    length
                }
            };
            if self.buffer.len() < body_bytes {
                return Ok(());
            }
            let body: Vec<u8> = self.buffer.drain(..body_bytes).collect();
            self.body_bytes = None;
            messages.push(parse_body(&body)?);
        }
    }

    pub fn end(&mut self) -> Result<(), ToolChildCodecError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self.buffer.is_empty() && self.body_bytes.is_none() {
            return Ok(());
        }
        let truncated = ToolChildCodecError::new(
            ToolChildCodecErrorCode::ToolChildTruncated,
            &format!(
                "stream ended inside a frame ({} of {} bytes)",
                self.buffer.len(),
                self.body_bytes.unwrap_or(FRAME_HEADER_BYTES)
            ),
        );
        self.fail(truncated.clone());
        Err(truncated)
    }

    fn fail(&mut self, error: ToolChildCodecError) {
        self.failure = Some(error);
        self.buffer = Vec::new();
        self.body_bytes = None;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::protocol::{
        ChildToServer, HostCallKind, LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin,
        PayloadRef, RefControl, RefNackReason, ServerToChild,
    };
    use crate::protocol_guard::tool_child_message_types;

    fn a_ref() -> PayloadRef {
        PayloadRef {
            ref_id: "r-1".into(),
            generation: 3,
            bytes: 10,
            sha256: "a".repeat(64),
        }
    }

    fn samples() -> Vec<ToolChildMessage> {
        use ChildToServer as C;
        use ServerToChild as S;
        let launch = |containment, id: &str| -> ToolChildMessage {
            C::Launch {
                launch_id: id.into(),
                job_id: "j1".into(),
                origin: LaunchOrigin::Tool,
                api: LaunchApi::BunSpawn,
                containment,
                lifetime: LaunchLifetime::Owned,
                pid: Some(10),
                pgid: Some(10),
            }
            .into()
        };
        vec![
            launch(LaunchContainment::OwnGroup, "l-own-group"),
            launch(LaunchContainment::Inherited, "l-inherited"),
            launch(LaunchContainment::Job, "l-job"),
            launch(LaunchContainment::Breakaway, "l-breakaway"),
            C::Launch {
                launch_id: "l-det".into(),
                job_id: "j1".into(),
                origin: LaunchOrigin::Kernel,
                api: LaunchApi::CpSpawn,
                containment: LaunchContainment::Breakaway,
                lifetime: LaunchLifetime::Detached,
                pid: None,
                pgid: None,
            }
            .into(),
            S::LaunchAck {
                launch_id: "l-1".into(),
            }
            .into(),
            C::LaunchProof {
                launch_id: "l-1".into(),
                ack_tick: Some(4),
                invoke_tick: 5,
            }
            .into(),
            C::LaunchProof {
                launch_id: "l-2".into(),
                ack_tick: None,
                invoke_tick: 6,
            }
            .into(),
            S::Ping { seq: 1 }.into(),
            C::Pong { seq: 1 }.into(),
            C::CancelAck {
                job_id: "j1".into(),
            }
            .into(),
            C::LaunchDone {
                launch_id: "l-1".into(),
                exit_code: Some(0),
                signal: None,
            }
            .into(),
            C::LaunchDone {
                launch_id: "l-2".into(),
                exit_code: None,
                signal: Some("SIGTERM".into()),
            }
            .into(),
            C::CommandStarted {
                launch_id: "l-1".into(),
                pid: 77,
                started_at_ms: 1234,
                creator_in_job: true,
                breakaway: false,
            }
            .into(),
            S::SlotQuery {
                launch_id: "l-1".into(),
            }
            .into(),
            C::SlotState {
                launch_id: "l-1".into(),
                ack_tick: None,
                invoke_tick: None,
                invocations: 0,
            }
            .into(),
            C::SlotState {
                launch_id: "l-1".into(),
                ack_tick: Some(4),
                invoke_tick: Some(5),
                invocations: 1,
            }
            .into(),
            C::Trace {
                at_ms: 1,
                event: "job_start".into(),
                detail: json!({"kind": "grep", "ok": true, "n": 3})
                    .as_object()
                    .cloned(),
            }
            .into(),
            C::Trace {
                at_ms: 2,
                event: "idle".into(),
                detail: None,
            }
            .into(),
            C::Batch {
                job_id: "j-parse".into(),
                done: false,
                entries: vec![
                    crate::protocol::ParsedEntryMeta {
                        id: "e1".into(),
                        parent_id: None,
                        line_offset: 0,
                        line_bytes: 120,
                        line_delta: 0,
                        entry: Some(json!({"type": "message"})),
                    },
                    crate::protocol::ParsedEntryMeta {
                        id: "e2".into(),
                        parent_id: Some("e1".into()),
                        line_offset: 121,
                        line_bytes: 4096,
                        line_delta: -3500,
                        entry: None,
                    },
                ],
            }
            .into(),
            C::Hello {
                generation: 3,
                pid: 99,
                protocol: 2,
                job_kinds: vec!["module".into()],
            }
            .into(),
            C::Update {
                job_id: "j1".into(),
                seq: 1,
                value: Some(json!({"index": 1})),
                value_ref: None,
            }
            .into(),
            S::UpdateAck {
                job_id: "j1".into(),
                seq: 1,
            }
            .into(),
            S::HelloAck { generation: 3 }.into(),
            C::Heartbeat { seq: 7 }.into(),
            S::Job {
                job_id: "j1".into(),
                kind: "read".into(),
                parent_job_id: None,
                args: Some(json!({"path": "a\nb.txt"})),
                args_ref: None,
            }
            .into(),
            S::Job {
                job_id: "j2".into(),
                kind: "write".into(),
                parent_job_id: Some("j1".into()),
                args: None,
                args_ref: Some(a_ref()),
            }
            .into(),
            S::Cancel {
                job_id: "j1".into(),
            }
            .into(),
            C::Result {
                job_id: "j1".into(),
                value: json!({"content": [{"type": "text", "text": "line\n"}]}),
            }
            .into(),
            C::Result {
                job_id: "j3".into(),
                value: Value::Null,
            }
            .into(),
            C::ResultRef {
                job_id: "j1".into(),
                r#ref: a_ref(),
            }
            .into(),
            C::JobError {
                job_id: "j1".into(),
                code: "tool_failed".into(),
                message: "boom".into(),
            }
            .into(),
            C::JobRefused {
                job_id: "j9".into(),
                code: "tool_child_jobs_full".into(),
            }
            .into(),
            C::HostCall {
                call_id: "c1".into(),
                job_id: "j1".into(),
                kind: HostCallKind::Tool,
                name: Some("read".into()),
                args: Some(json!({"path": "x"})),
                args_ref: None,
            }
            .into(),
            C::HostCall {
                call_id: "c2".into(),
                job_id: "j1".into(),
                kind: HostCallKind::Tool,
                name: Some("write".into()),
                args: None,
                args_ref: Some(a_ref()),
            }
            .into(),
            C::HostCall {
                call_id: "c3".into(),
                job_id: "j1".into(),
                kind: HostCallKind::ListTools,
                name: None,
                args: None,
                args_ref: None,
            }
            .into(),
            S::HostReply {
                call_id: "c1".into(),
                value: Some(json!([1, 2])),
                value_ref: None,
                error: None,
            }
            .into(),
            S::HostReply {
                call_id: "c2".into(),
                value: None,
                value_ref: Some(a_ref()),
                error: None,
            }
            .into(),
            S::HostReply {
                call_id: "c3".into(),
                value: None,
                value_ref: None,
                error: Some("denied".into()),
            }
            .into(),
            S::HostCallRefused {
                call_id: "c265".into(),
                code: "host_call_capacity".into(),
            }
            .into(),
            C::Wake {
                wake_id: "w1".into(),
                source: "monitor".into(),
                bytes: 12,
                value: None,
            }
            .into(),
            S::WakeAck {
                wake_id: "w1".into(),
            }
            .into(),
            RefControl::RefAck {
                ref_id: "r-1".into(),
            }
            .into(),
            RefControl::RefNack {
                ref_id: "r-1".into(),
                reason: RefNackReason::Cancelled,
            }
            .into(),
            S::Shutdown.into(),
        ]
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn codec_error<T>(run: Result<T, ToolChildCodecError>) -> Option<ToolChildCodecErrorCode> {
        run.err().map(|error| error.code)
    }

    fn frame_with_body(body: &str) -> Vec<u8> {
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(body.as_bytes());
        frame
    }

    fn encode(message: ToolChildMessage) -> Vec<u8> {
        encode_frame(&message).unwrap()
    }

    #[test]
    fn round_trips_every_message_type_through_one_chunked_stream() {
        let samples = samples();
        let seen: BTreeSet<&str> = samples.iter().map(ToolChildMessage::message_type).collect();
        let expected: BTreeSet<&str> = tool_child_message_types().into_iter().collect();
        assert_eq!(seen, expected);
        let stream = concat(&samples.iter().cloned().map(encode).collect::<Vec<_>>());
        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::new();
        for chunk in stream.chunks(7) {
            decoded.extend(decoder.push(chunk).unwrap());
        }
        decoder.end().unwrap();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn frames_with_a_big_endian_length_prefix_not_ndjson() {
        let frame = encode(
            ServerToChild::Job {
                job_id: "j1".into(),
                kind: "read".into(),
                parent_job_id: None,
                args: Some(json!({"text": "a\nb"})),
                args_ref: None,
            }
            .into(),
        );
        let body_bytes = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        assert_eq!(frame.len(), FRAME_HEADER_BYTES + body_bytes);
        assert_ne!(frame.last().copied(), Some(0x0a));
    }

    #[test]
    fn accepts_a_body_of_exactly_the_cap_and_refuses_one_byte_more_on_encode() {
        let overhead = json!({"type": "result", "jobId": "j", "value": ""})
            .to_string()
            .len();
        let fits = "x".repeat(TOOL_CHILD_LIMITS.frame_bytes - overhead);
        let message = |value: &str| -> ToolChildMessage {
            ChildToServer::Result {
                job_id: "j".into(),
                value: Value::String(value.into()),
            }
            .into()
        };
        let frame = encode_frame(&message(&fits)).unwrap();
        assert_eq!(
            FrameDecoder::new().push(&frame).unwrap(),
            vec![message(&fits)]
        );
        assert_eq!(
            codec_error(encode_frame(&message(&format!("{fits}x")))),
            Some(ToolChildCodecErrorCode::ToolChildFrameTooLarge)
        );
    }

    #[test]
    fn refuses_a_16_mib_plus_1_length_header_before_allocating_its_body_and_stays_failed() {
        let header = ((TOOL_CHILD_LIMITS.frame_bytes + 1) as u32).to_be_bytes();
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            codec_error(decoder.push(&header)),
            Some(ToolChildCodecErrorCode::ToolChildFrameTooLarge)
        );
        assert_eq!(decoder.buffered_bytes(), 0);
        let shutdown = encode(ServerToChild::Shutdown.into());
        assert_eq!(
            codec_error(decoder.push(&shutdown)),
            Some(ToolChildCodecErrorCode::ToolChildFrameTooLarge)
        );
    }

    #[test]
    fn reports_a_truncated_stream_as_tool_child_truncated() {
        let frame = encode(ChildToServer::Heartbeat { seq: 1 }.into());
        let mut partial_body = FrameDecoder::new();
        assert!(
            partial_body
                .push(&frame[..frame.len() - 1])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            codec_error(partial_body.end()),
            Some(ToolChildCodecErrorCode::ToolChildTruncated)
        );
        let mut partial_header = FrameDecoder::new();
        partial_header.push(&frame[..2]).unwrap();
        assert_eq!(
            codec_error(partial_header.end()),
            Some(ToolChildCodecErrorCode::ToolChildTruncated)
        );
        let mut clean = FrameDecoder::new();
        clean.push(&frame).unwrap();
        assert_eq!(codec_error(clean.end()), None);
    }

    #[test]
    fn refuses_malformed_frames_instead_of_dropping_them() {
        let cases = [
            "not json".to_owned(),
            json!({"type": "unknown_type"}).to_string(),
            json!({"type": "launch_proof", "launchId": "l", "ackTick": 1}).to_string(),
            json!({"type": "trace", "atMs": 1, "event": "e", "value": {"result": 1}}).to_string(),
            json!({"type": "trace", "atMs": 1, "event": "e", "detail": {"ackTick": 4}}).to_string(),
            json!({"type": "launch", "launchId": "l", "jobId": "j", "origin": "tool", "api": "bun.spawn",
                   "containment": "pid", "lifetime": "owned"})
            .to_string(),
        ];
        for body in cases {
            assert_eq!(
                codec_error(FrameDecoder::new().push(&frame_with_body(&body))),
                Some(ToolChildCodecErrorCode::ToolChildMalformed),
                "{body}"
            );
        }
    }

    #[test]
    fn optional_fields_left_none_are_dropped_and_still_encode() {
        let message: ToolChildMessage = ChildToServer::LaunchDone {
            launch_id: "l1".into(),
            exit_code: None,
            signal: Some("SIGTERM".into()),
        }
        .into();
        let frame = encode(message.clone());
        let decoded = FrameDecoder::new().push(&frame).unwrap();
        assert_eq!(decoded, vec![message]);
        assert!(!String::from_utf8_lossy(&frame[FRAME_HEADER_BYTES..]).contains("exitCode"));
    }

    #[test]
    fn a_job_result_of_null_encodes_as_an_explicit_null_the_decoder_accepts() {
        let decoded = FrameDecoder::new().push(&encode(
            ChildToServer::Result {
                job_id: "j".into(),
                value: Value::Null,
            }
            .into(),
        ));
        assert_eq!(
            decoded.unwrap(),
            vec![ToolChildMessage::from(ChildToServer::Result {
                job_id: "j".into(),
                value: Value::Null
            })]
        );
    }
}
