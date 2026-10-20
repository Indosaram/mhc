//! Client adapters for the shared Chord wire and delta codecs.
use super::errors::ClientError;
use maho_chord::{services::{state_codec, wire}, types::JsonValue};
use serde::{Serialize, de::DeserializeOwned};
use maho_chord::types::{JsString, JsonValue as Value};

fn protocol(error: impl std::fmt::Display) -> ClientError {
    ClientError::Protocol(error.to_string())
}

// Text is intentional: serde Value cannot represent isolated UTF-16 surrogates.
// Lossless callers use JsonValue throughout; legacy Value callers fail rather than
// replacing a surrogate.
fn lossless(value: &impl Serialize) -> Result<JsonValue, ClientError> {
    serde_json::from_str(&serde_json::to_string(value).map_err(protocol)?).map_err(protocol)
}
fn output<T: DeserializeOwned>(value: &impl Serialize) -> Result<T, ClientError> {
    serde_json::from_str(&serde_json::to_string(value).map_err(protocol)?).map_err(protocol)
}

pub fn parse_service_call(v: &impl Serialize) -> Result<(), ClientError> {
    wire::parse_service_call(lossless(v)?).map(|_| ()).map_err(protocol)
}
pub fn parse_catalogue(v: &impl Serialize) -> Result<(), ClientError> {
    wire::parse_service_catalogue(lossless(v)?).map(|_| ()).map_err(protocol)
}
pub fn parse_service_subscription_snapshot(v: &impl Serialize) -> Result<(), ClientError> {
    wire::parse_service_subscription_snapshot(lossless(v)?).map(|_| ()).map_err(protocol)
}
pub fn parse_wire_service_subscription_snapshot(v: &impl Serialize) -> Result<(), ClientError> {
    wire::parse_wire_service_subscription_snapshot(lossless(v)?).map(|_| ()).map_err(protocol)
}
pub fn parse_service_provider_update(v: &impl Serialize) -> Result<(), ClientError> {
    wire::parse_service_provider_update(lossless(v)?).map(|_| ()).map_err(protocol)
}
pub fn parse_wire_service_provider_update(v: &impl Serialize) -> Result<(), ClientError> {
    wire::parse_wire_service_provider_update(lossless(v)?).map(|_| ()).map_err(protocol)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceControlCall {
    Catalogue,
    Subscribe { subscription_id: JsString, service_id: JsString, mode: maho_chord::types::ServiceMode },
    Unsubscribe { subscription_id: JsString },
}
pub fn create_service_catalogue_call() -> Result<Value, ClientError> {
    output(&wire::create_service_catalogue_call())
}
pub fn create_service_subscribe_call(subscription_id: &JsString, service_id: &JsString, mode: maho_chord::types::ServiceMode) -> Result<Value, ClientError> {
    output(&wire::create_service_subscribe_call(subscription_id, service_id, mode))
}
pub fn create_service_unsubscribe_call(subscription_id: &JsString) -> Result<Value, ClientError> {
    output(&wire::create_service_unsubscribe_call(subscription_id))
}
pub fn decode_service_control_call(call: &impl Serialize) -> Option<ServiceControlCall> {
    let call = wire::parse_service_call(lossless(call).ok()?).ok()?;
    match wire::decode_service_control_call(&call)? {
        wire::ServiceControlCall::Catalogue => Some(ServiceControlCall::Catalogue),
        wire::ServiceControlCall::Subscribe { subscription_id, service_id, mode } => {
            Some(ServiceControlCall::Subscribe { subscription_id, service_id, mode })
        }
        wire::ServiceControlCall::Unsubscribe { subscription_id } => Some(ServiceControlCall::Unsubscribe { subscription_id }),
    }
}

pub struct ServiceStateEncoder {
    inner: state_codec::ServiceStateEncoder,
}
impl Default for ServiceStateEncoder {
    fn default() -> Self {
        Self { inner: state_codec::create_service_state_encoder() }
    }
}
impl ServiceStateEncoder {
    pub fn snapshot(&mut self, value: &JsonValue) -> Result<JsonValue, crate::server::errors::ServerError> {
        let snapshot = wire::parse_service_subscription_snapshot(value.clone()).map_err(|e| crate::server::errors::ServerError::new("invalid_request", &e.to_string()))?;
        let snapshot = self.inner.encode_snapshot(snapshot).map_err(|e| crate::server::errors::ServerError::new("invalid_request", &e.to_string()))?;
        output(&snapshot).map_err(|e| crate::server::errors::ServerError::new("invalid_request", &e.to_string()))
    }
    pub fn update(&mut self, value: &JsonValue) -> Result<JsonValue, crate::server::errors::ServerError> {
        let update = wire::parse_service_provider_update(value.clone()).map_err(|e| crate::server::errors::ServerError::new("invalid_request", &e.to_string()))?;
        let update = self.inner.encode_update(update).map_err(|e| crate::server::errors::ServerError::new("invalid_request", &e.to_string()))?;
        output(&update).map_err(|e| crate::server::errors::ServerError::new("invalid_request", &e.to_string()))
    }
}

pub struct ServiceStateDecoder {
    inner: state_codec::ServiceStateDecoder,
}
impl Default for ServiceStateDecoder {
    fn default() -> Self {
        Self { inner: state_codec::create_service_state_decoder() }
    }
}
impl ServiceStateDecoder {
    pub fn snapshot<T: Serialize + DeserializeOwned>(&mut self, value: &T) -> Result<T, ClientError> {
        let snapshot = wire::parse_wire_service_subscription_snapshot(lossless(value)?).map_err(protocol)?;
        output(&self.inner.decode_snapshot(snapshot).map_err(protocol)?)
    }
    pub fn update<T: Serialize + DeserializeOwned>(&mut self, value: &T) -> Result<T, ClientError> {
        let update = wire::parse_wire_service_provider_update(lossless(value)?).map_err(protocol)?;
        output(&self.inner.decode_update(update).map_err(protocol)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_ids_and_large_service_sequences_remain_native() {
        let subscription = JsString::from_utf16(vec![0xd800]);
        let service = JsString::from_utf16(vec![0xdfff]);
        let call = create_service_subscribe_call(&subscription, &service, maho_chord::types::ServiceMode::Keyed).expect("control call");
        assert_eq!(decode_service_control_call(&call), Some(ServiceControlCall::Subscribe { subscription_id: subscription, service_id: service.clone(), mode: maho_chord::types::ServiceMode::Keyed }));
        let snapshot = parse(r#"{"serviceId":"\udfff","mode":"singleton","instances":[{"members":[{"kind":"state","name":"\ud800","sequence":18446744073709551616,"ops":[["r",{"\ud800":"\udfff"}]]}]}]}"#);
        let mut encoder = ServiceStateEncoder::default();
        let wire = encoder.snapshot(&snapshot).expect("native encode");
        let decoded = ServiceStateDecoder::default().snapshot(&wire).expect("native decode");
        assert_eq!(decoded["serviceId"], JsonValue::String(service));
        assert_eq!(decoded["instances"][0]["members"][0]["ops"], snapshot["instances"][0]["members"][0]["ops"]);
        let sequence: maho_chord::types::ServiceSequence = serde_json::from_str("18446744073709551616").expect("integral JS Number");
        let decoded_sequence: maho_chord::types::ServiceSequence = serde_json::from_str(&serde_json::to_string(&decoded["instances"][0]["members"][0]["sequence"]).expect("sequence JSON")).expect("decoded sequence");
        assert_eq!(decoded_sequence, sequence);
    }

    fn parse(text: &str) -> JsonValue {
        serde_json::from_str(text).expect("lossless fixture")
    }

    #[test]
    fn decodes_root_permutations_short_forms_and_surrogate_paths() {
        // Given a root-array state and a persistent wire decoder.
        let mut decoder = ServiceStateDecoder::default();
        let snapshot = parse(r#"{"serviceId":"lane","mode":"singleton","instances":[{"members":[{"kind":"state","name":"value","sequence":0,"ops":[["r",[1,2,3]]]}]}]}"#);
        decoder.snapshot(&snapshot).expect("snapshot");
        let update = parse(r#"{"type":"state","member":"value","sequence":1,"ops":[["#",0,[]],["m",0,[2,0,1]],["p",0,0,[4]],["t",2],["#",1,[0,"\ud800"]],["s",1,"\udfff"],["a","\ud800"]]}"#);
        // When the shared wire decoder expands root and repeated paths.
        let decoded = decoder.update(&update).expect("update");
        // Then every operation and isolated surrogate remains intact.
        let expected = parse(r#"[["m",[],[2,0,1]],["p",[],0,0,[4]],["t",[],2],["s",[0,"\ud800"],"\udfff"],["a",[0,"\ud800"],"\ud800"]]"#);
        assert_eq!(decoded["ops"], expected);
    }

    #[test]
    fn reset_discards_prior_path_dictionary_and_preserves_surrogate_keys() {
        // Given a state whose previous frame defined a path dictionary entry.
        let mut decoder = ServiceStateDecoder::default();
        let snapshot = parse(r#"{"serviceId":"lane","mode":"singleton","instances":[{"members":[{"kind":"state","name":"value","sequence":0,"ops":[["r",{}],["#",0,["old"]],["s",0,1]]}]}]}"#);
        decoder.snapshot(&snapshot).expect("snapshot");
        let reset = parse(r#"{"type":"reset","snapshot":{"serviceId":"lane","mode":"singleton","instances":[{"members":[{"kind":"state","name":"value","sequence":0,"ops":[["r",{"\ud800":"\udfff"}]]}]}]}}"#);
        // When reset establishes a replacement baseline.
        let decoded = decoder.update(&reset).expect("reset");
        // Then its payload is lossless and old dictionary references are invalid.
        assert_eq!(decoded, reset);
        let stale = parse(r#"{"type":"state","member":"value","sequence":1,"ops":[["s",0,2]]}"#);
        assert!(decoder.update(&stale).is_err());
    }
}
