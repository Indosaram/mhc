//! Rust port of `otlp-proto/opentelemetry/proto/collector/logs/v1/logs_service_pb.ts`.

use super::ProtoError;
use super::Reader;
use super::WIRE_LEN;
use super::WIRE_VARINT;
use super::logs::ResourceLogs;
use super::logs::decode_resource_logs;
use super::logs::encode_resource_logs;
use super::write_message;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportLogsServiceRequest {
    pub resource_logs: Vec<ResourceLogs>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportLogsPartialSuccess {
    pub rejected_log_records: i64,
    pub error_message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportLogsServiceResponse {
    pub partial_success: Option<ExportLogsPartialSuccess>,
}

pub fn encode_export_logs_service_request(request: &ExportLogsServiceRequest) -> Vec<u8> {
    let mut out = Vec::new();
    for resource_logs in &request.resource_logs {
        write_message(&mut out, 1, &encode_resource_logs(resource_logs));
    }
    out
}

pub fn decode_export_logs_service_request(
    bytes: &[u8],
) -> Result<ExportLogsServiceRequest, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut request = ExportLogsServiceRequest::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => request
                .resource_logs
                .push(decode_resource_logs(reader.read_len()?)?),
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(request)
}

pub fn decode_export_logs_service_response(
    bytes: &[u8],
) -> Result<ExportLogsServiceResponse, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut response = ExportLogsServiceResponse::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => {
                let body = reader.read_len()?;
                let mut nested = Reader::new(body);
                let mut partial = ExportLogsPartialSuccess::default();
                while !nested.is_empty() {
                    let (nested_field, nested_wire) = nested.read_tag()?;
                    match (nested_field, nested_wire) {
                        (1, WIRE_VARINT) => partial.rejected_log_records = nested.read_varint()? as i64,
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
