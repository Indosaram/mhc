//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/config.ts.

use super::coerce_parameters::coerce_antml_parameters;
use crate::tool_call_middleware::protocols::anthropic_xml::invoke_protocol::InvokeProtocolConfig;

pub const ANTML_INVOKE_CONFIG: InvokeProtocolConfig = InvokeProtocolConfig {
    protocol: "antml",
    label: "antml",
    id_prefix: "antml-tool",
    coerce: coerce_antml_parameters,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_the_antml_protocol_identity() {
        assert_eq!(ANTML_INVOKE_CONFIG.protocol, "antml");
        assert_eq!(ANTML_INVOKE_CONFIG.label, "antml");
        assert_eq!(ANTML_INVOKE_CONFIG.id_prefix, "antml-tool");
    }
}
