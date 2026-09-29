//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/invoke-protocol.ts.

use super::coerce_parameters::coerce_parameters;
use super::invoke_tag_syntax::InvokeParameter;
use crate::types::Tool;
use serde_json::{Map, Value};

pub struct InvokeProtocolConfig {
    pub protocol: &'static str,
    pub label: &'static str,
    pub id_prefix: &'static str,
    pub coerce: fn(&[InvokeParameter], &Tool) -> Option<Map<String, Value>>,
}

pub const ANTHROPIC_XML_INVOKE_CONFIG: InvokeProtocolConfig =
    InvokeProtocolConfig { protocol: "anthropic-xml", label: "Anthropic XML", id_prefix: "anthropic-xml-tool", coerce: coerce_parameters };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_xml_invoke_config_has_expected_identity() {
        assert_eq!(ANTHROPIC_XML_INVOKE_CONFIG.protocol, "anthropic-xml");
        assert_eq!(ANTHROPIC_XML_INVOKE_CONFIG.label, "Anthropic XML");
        assert_eq!(ANTHROPIC_XML_INVOKE_CONFIG.id_prefix, "anthropic-xml-tool");
    }
}
