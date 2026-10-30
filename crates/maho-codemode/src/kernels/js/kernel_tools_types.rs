pub use super::kernel_tools_errors::{
    KernelToolErrorCode, KernelToolHostDenial, KernelToolHostDenialReason,
};
pub use maho_ext_api::{
    KernelToolInvokeOptions as KernelToolsInvokeOptions,
    KernelToolInvokeRequest as KernelToolsInvokeRequest,
    KernelToolInvokeScope as KernelToolsInvokeScope,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub language: String,
    pub kernel_generation: u64,
    pub definition_revision: u64,
}

pub const KERNEL_TOOLS_INVOKE_SCOPE: bool = true;
pub const KERNEL_TOOLS_UNSUPPORTED_MESSAGE: &str =
    "Kernel tools require a live JavaScript worker context";

/// Message of a `describe` entry or invoke failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelToolsDescribeError {
    pub code: KernelToolErrorCode,
    pub message: String,
}

/// senpi `KernelToolsDescribeEntry`: `ok: true` carries a descriptor, `ok: false` an error.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KernelToolsDescribeEntry {
    Found {
        name: String,
        ok: monostate_true::True,
        descriptor: KernelToolDescriptor,
    },
    Failed {
        name: String,
        ok: monostate_true::False,
        error: KernelToolsDescribeError,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelToolsDescribeResult {
    pub results: Vec<KernelToolsDescribeEntry>,
}

/// Stable capability markers a consumer gates on before sending an option this runtime may not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelToolsCapabilities {
    pub invoke_scope: bool,
}

pub const KERNEL_TOOLS_CAPABILITIES: KernelToolsCapabilities = KernelToolsCapabilities {
    invoke_scope: KERNEL_TOOLS_INVOKE_SCOPE,
};

pub const KERNEL_TOOLS_UNSUPPORTED: KernelToolsDescribeErrorRef = KernelToolsDescribeErrorRef {
    code: KernelToolErrorCode::ToolsUnavailable,
    message: KERNEL_TOOLS_UNSUPPORTED_MESSAGE,
};

/// Const form of [`KernelToolsDescribeError`] for `KERNEL_TOOLS_UNSUPPORTED`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelToolsDescribeErrorRef {
    pub code: KernelToolErrorCode,
    pub message: &'static str,
}

/// Boolean literal markers so the untagged union only matches its own `ok` value.
pub mod monostate_true {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct True;
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct False;

    fn literal<'de, D: Deserializer<'de>>(deserializer: D, expected: bool) -> Result<(), D::Error> {
        if bool::deserialize(deserializer)? == expected {
            Ok(())
        } else {
            Err(serde::de::Error::custom(format!("expected ok: {expected}")))
        }
    }
    impl Serialize for True {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bool(true)
        }
    }
    impl<'de> Deserialize<'de> for True {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            literal(deserializer, true).map(|()| True)
        }
    }
    impl Serialize for False {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bool(false)
        }
    }
    impl<'de> Deserialize<'de> for False {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            literal(deserializer, false).map(|()| False)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_result_round_trips_both_union_arms_by_ok() {
        let wire = serde_json::json!({"results":[
            {"name":"a","ok":true,"descriptor":{"name":"a","description":"d","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":2}},
            {"name":"b","ok":false,"error":{"code":"kernel_tool_missing","message":"gone"}}
        ]});
        let parsed: KernelToolsDescribeResult =
            serde_json::from_value(wire.clone()).expect("parse");
        assert!(
            matches!(&parsed.results[0], KernelToolsDescribeEntry::Found { descriptor, .. } if descriptor.definition_revision == 2)
        );
        assert!(
            matches!(&parsed.results[1], KernelToolsDescribeEntry::Failed { error, .. } if error.code == KernelToolErrorCode::KernelToolMissing)
        );
        assert_eq!(serde_json::to_value(&parsed).expect("serialize"), wire);
    }

    #[test]
    fn capabilities_and_unsupported_constants_match_upstream() {
        assert_eq!(
            serde_json::to_value(KERNEL_TOOLS_CAPABILITIES).expect("caps"),
            serde_json::json!({"invokeScope":true})
        );
        assert_eq!(
            KERNEL_TOOLS_UNSUPPORTED.code,
            KernelToolErrorCode::ToolsUnavailable
        );
        assert_eq!(
            KERNEL_TOOLS_UNSUPPORTED.message,
            "Kernel tools require a live JavaScript worker context"
        );
    }
}
