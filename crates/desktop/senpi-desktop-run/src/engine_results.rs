//! Native Rust port of `src/run/engine-results.ts`.
//!
//! Result shapes of the engine methods the run facade calls, mirrored from `senpi-desktop-core`.
//! Engine stdout is the trust boundary: every result is shape-checked here.

use senpi_desktop_service::rpc_client::{DesktopError, DesktopServiceError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Monitor geometry in global logical points plus composite screenshot pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDisplay {
    pub id: String,
    pub name: String,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub scale: f64,
    pub pixel_x: i64,
    pub pixel_y: i64,
    pub pixel_width: i64,
    pub pixel_height: i64,
    pub is_primary: bool,
}

/// One top-level window in global logical points; `id` is opaque.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWindow {
    pub id: String,
    pub title: String,
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevated: Option<bool>,
}

/// One accessibility element; bounds are global logical points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNode {
    #[serde(rename = "ref")]
    pub ref_id: String,
    pub role: String,
    pub native_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<String>>,
    pub child_count: usize,
}

// In JSON wire, "ref" is used instead of "ref_id"
impl<'de> serde::Deserialize<'de> for AxNodeCustomHelper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            #[serde(alias = "ref", alias = "refId")]
            ref_id: String,
            role: String,
            native_role: String,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            value: Option<String>,
            #[serde(default)]
            description: Option<String>,
            enabled: bool,
            focused: bool,
            #[serde(default)]
            x: Option<i64>,
            #[serde(default)]
            y: Option<i64>,
            #[serde(default)]
            width: Option<i64>,
            #[serde(default)]
            height: Option<i64>,
            #[serde(default)]
            actions: Option<Vec<String>>,
            child_count: usize,
        }
        let w = Wire::deserialize(deserializer)?;
        Ok(AxNodeCustomHelper(AxNode {
            ref_id: w.ref_id,
            role: w.role,
            native_role: w.native_role,
            title: w.title,
            value: w.value,
            description: w.description,
            enabled: w.enabled,
            focused: w.focused,
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
            actions: w.actions,
            child_count: w.child_count,
        }))
    }
}

struct AxNodeCustomHelper(AxNode);

/// The `capture` result: inline image bytes, or only an artifact path when over byte budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    pub width: i64,
    pub height: i64,
    pub source_width: i64,
    pub source_height: i64,
    pub scale: f64,
    pub target: String,
    pub frame_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextResult {
    pub text: String,
}

pub fn expect_result<T: for<'de> Deserialize<'de>>(
    method: &str,
    value: Value,
) -> Result<T, DesktopError> {
    serde_json::from_value::<T>(value).map_err(|e| {
        DesktopError::Service(DesktopServiceError::internal(format!(
            "desktop engine sent a malformed {method} result: {e}"
        )))
    })
}

pub fn expect_ax_node(method: &str, value: Value) -> Result<AxNode, DesktopError> {
    serde_json::from_value::<AxNodeCustomHelper>(value)
        .map(|h| h.0)
        .map_err(|e| {
            DesktopError::Service(DesktopServiceError::internal(format!(
                "desktop engine sent a malformed {method} result: {e}"
            )))
        })
}

pub fn expect_optional_ax_node(
    method: &str,
    value: Value,
) -> Result<Option<AxNode>, DesktopError> {
    if value.is_null() {
        return Ok(None);
    }
    expect_ax_node(method, value).map(Some)
}

pub fn expect_ax_nodes(method: &str, value: Value) -> Result<Vec<AxNode>, DesktopError> {
    let list: Vec<AxNodeCustomHelper> = expect_result(method, value)?;
    Ok(list.into_iter().map(|h| h.0).collect())
}

fn malformed(method: &str, e: &serde_json::Error) -> DesktopError {
    DesktopError::Service(DesktopServiceError::internal(format!(
        "desktop engine sent a malformed {method} result: {e}"
    )))
}

/// Shape check of [`expect_result`] that borrows `value` instead of consuming it.
pub(crate) fn check_result<T: for<'de> Deserialize<'de>>(
    method: &str,
    value: &Value,
) -> Result<(), DesktopError> {
    T::deserialize(value).map(|_| ()).map_err(|e| malformed(method, &e))
}

/// Shape check of [`expect_ax_node`] that borrows `value`.
pub(crate) fn check_ax_node(method: &str, value: &Value) -> Result<(), DesktopError> {
    check_result::<AxNodeCustomHelper>(method, value)
}

/// Shape check of [`expect_ax_nodes`] that borrows `value`.
pub(crate) fn check_ax_nodes(method: &str, value: &Value) -> Result<(), DesktopError> {
    check_result::<Vec<AxNodeCustomHelper>>(method, value)
}

/// Validates a JSON string without copying it (same error text as `String`).
struct StrCheck;

impl<'de> Deserialize<'de> for StrCheck {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = StrCheck;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<StrCheck, E> {
                Ok(StrCheck)
            }
        }
        deserializer.deserialize_str(V)
    }
}

/// Field-for-field mirror of [`CaptureResult`] whose `data` is validated, not copied.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CaptureResultCheck {
    mode: StrCheck,
    #[serde(default)]
    data: Option<StrCheck>,
    #[serde(default)]
    mime_type: Option<StrCheck>,
    #[serde(default)]
    artifact_path: Option<StrCheck>,
    width: i64,
    height: i64,
    source_width: i64,
    source_height: i64,
    scale: f64,
    target: StrCheck,
    frame_id: StrCheck,
    #[serde(default)]
    note: Option<StrCheck>,
}

/// Shape check of `expect_result::<CaptureResult>` that never copies the image data.
pub(crate) fn check_capture(method: &str, value: &Value) -> Result<(), DesktopError> {
    check_result::<CaptureResultCheck>(method, value)
}
