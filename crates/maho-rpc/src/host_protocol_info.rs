use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRuntimeKind {
    #[serde(rename = "in-process")]
    InProcess,
    #[serde(rename = "worker")]
    Worker,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcLaunchProfileCore {
    pub extensions: Vec<String>,
    pub multi_session: bool,
    pub session_runtime: SessionRuntimeKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcLaunchProfile {
    pub profile_id: String,
    pub core: RpcLaunchProfileCore,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostProtocolInfo {
    pub protocol_version: f64,
    pub server_version: String,
    pub capabilities: Vec<String>,
    pub instance_id: Option<String>,
    pub generation: Option<f64>,
    pub engine_version: Option<String>,
    pub engine_ordinal: Option<[f64; 5]>,
    pub launch_profile: Option<RpcLaunchProfile>,
}

pub fn parse_host_protocol_info(data: &Value) -> Option<HostProtocolInfo> {
    let object = data.as_object()?;
    let server_version = object.get("serverVersion")?.as_str()?.to_owned();
    let capabilities = object.get("capabilities")?.as_array()?.iter()
        .map(|value| value.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?;
    let engine_ordinal = object.get("engineOrdinal").and_then(Value::as_array)
        .filter(|parts| parts.len() == 5)
        .and_then(|parts| parts.iter().map(Value::as_f64).collect::<Option<Vec<_>>>())
        .and_then(|parts| parts.try_into().ok());
    let launch_profile = object.get("launch_profile").and_then(|value| {
        let profile_id = value.get("profile_id")?.as_str()?.to_owned();
        let core = value.get("core")?.as_object()?;
        let extensions = core.get("extensions")?.as_array()?.iter()
            .map(|value| value.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?;
        let multi_session = core.get("multi_session")?.as_bool()?;
        let session_runtime = match core.get("session_runtime")?.as_str()? {
            "in-process" => SessionRuntimeKind::InProcess,
            "worker" => SessionRuntimeKind::Worker,
            _ => return None,
        };
        Some(RpcLaunchProfile { profile_id, core: RpcLaunchProfileCore { extensions, multi_session, session_runtime } })
    });
    Some(HostProtocolInfo {
        protocol_version: object.get("protocolVersion").and_then(Value::as_f64).unwrap_or(0.0),
        server_version,
        capabilities,
        instance_id: object.get("instanceId").and_then(Value::as_str).map(str::to_owned),
        generation: object.get("generation").and_then(Value::as_f64)
            .filter(|value| value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0),
        engine_version: object.get("engineVersion").and_then(Value::as_str).map(str::to_owned),
        engine_ordinal,
        launch_profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_host_keeps_identity_absent_and_protocol_zero() {
        let data = json!({"serverVersion":"old","capabilities":[]});
        let host = parse_host_protocol_info(&data).unwrap();
        assert!(host.protocol_version.abs() < f64::EPSILON);
        assert_eq!(host.generation, None);
        assert_eq!(host.engine_ordinal, None);
        assert_eq!(host.launch_profile, None);
    }

    #[test]
    fn malformed_required_fields_reject_the_reply() {
        for data in [Value::Null, json!([]), json!({"capabilities":[]}), json!({"serverVersion":1,"capabilities":[]}), json!({"serverVersion":"old","capabilities":[1]})] {
            assert!(parse_host_protocol_info(&data).is_none());
        }
    }

    #[test]
    fn malformed_identity_fields_are_dropped_not_defaulted() {
        let data = json!({"serverVersion":"old","capabilities":[],"instanceId":1,"generation":0.5,"engineVersion":false,"engineOrdinal":[1,2,3,4],"launch_profile":{"profile_id":"bad","core":{"extensions":[],"multi_session":true,"session_runtime":"invalid"}}});
        let host = parse_host_protocol_info(&data).unwrap();
        assert_eq!(host.instance_id, None);
        assert_eq!(host.generation, None);
        assert_eq!(host.engine_version, None);
        assert_eq!(host.engine_ordinal, None);
        assert_eq!(host.launch_profile, None);
    }

    #[test]
    fn valid_identity_and_launch_profile_are_preserved() {
        let data = json!({"serverVersion":"info","protocolVersion":1,"capabilities":["multi_session"],"instanceId":"host","generation":-1,"engineVersion":"build","engineOrdinal":[2026,9,16,3,0],"launch_profile":{"profile_id":"profile","core":{"extensions":["/plugin"],"multi_session":true,"session_runtime":"worker"}}});
        let host = parse_host_protocol_info(&data).unwrap();
        assert_eq!(host.instance_id.as_deref(), Some("host"));
        assert_eq!(host.engine_ordinal, Some([2026.0,9.0,16.0,3.0,0.0]));
        assert_eq!(host.launch_profile.unwrap().core.session_runtime, SessionRuntimeKind::Worker);
    }

    #[test]
    fn unsafe_generation_is_omitted() {
        let data = json!({"serverVersion":"info","capabilities":[],"generation":9_007_199_254_740_992_u64});
        assert_eq!(parse_host_protocol_info(&data).unwrap().generation,None);
    }

    #[test]
    fn ordinal_accepts_numeric_parts_without_integer_coercion() {
        let data = json!({"serverVersion":"info","capabilities":[],"engineOrdinal":[2026,9,16,3,0.5]});
        assert_eq!(parse_host_protocol_info(&data).unwrap().engine_ordinal,Some([2026.0,9.0,16.0,3.0,0.5]));
    }
}
