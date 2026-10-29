//! Rust port of `otlp-proto/opentelemetry/proto/collector/trace/v1/trace_service_pb.ts`.

use super::ProtoError;
use super::Reader;
use super::WIRE_LEN;
use super::WIRE_VARINT;
use super::trace::ResourceSpans;
use super::trace::decode_resource_spans;
use super::trace::encode_resource_spans;
use super::write_message;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportTraceServiceRequest {
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportTracePartialSuccess {
    pub rejected_spans: i64,
    pub error_message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportTraceServiceResponse {
    pub partial_success: Option<ExportTracePartialSuccess>,
}

pub fn encode_export_trace_service_request(request: &ExportTraceServiceRequest) -> Vec<u8> {
    let mut out = Vec::new();
    for resource_spans in &request.resource_spans {
        write_message(&mut out, 1, &encode_resource_spans(resource_spans));
    }
    out
}

pub fn decode_export_trace_service_request(
    bytes: &[u8],
) -> Result<ExportTraceServiceRequest, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut request = ExportTraceServiceRequest::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => request
                .resource_spans
                .push(decode_resource_spans(reader.read_len()?)?),
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(request)
}

pub fn decode_export_trace_service_response(
    bytes: &[u8],
) -> Result<ExportTraceServiceResponse, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut response = ExportTraceServiceResponse::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => {
                let body = reader.read_len()?;
                let mut nested = Reader::new(body);
                let mut partial = ExportTracePartialSuccess::default();
                while !nested.is_empty() {
                    let (nested_field, nested_wire) = nested.read_tag()?;
                    match (nested_field, nested_wire) {
                        (1, WIRE_VARINT) => partial.rejected_spans = nested.read_varint()? as i64,
                        (2, WIRE_LEN) => partial.error_message = nested.read_string()?,
                        (_, nested_wire) => nested.skip(nested_wire)?,
                    }
                }
                response.partial_success = Some(partial);
            }
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(response)
}
