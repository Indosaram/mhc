//! Native Rust port of `src/run/screenshot.ts`.
//!
//! Captures screenshots within the snapshot byte budget and coordinate frame.

use senpi_desktop_protocol::ComputerScreenshot;
use senpi_desktop_service::rpc_client::DesktopError;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::context::RunScope;
use crate::engine_results::{expect_result, CaptureResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenshotOptions {
    /// Skip displaying the capture to the model.
    #[serde(default)]
    pub silent: bool,
}

/// What `screenshot()` returns to user code; `x`/`y` of later input are pixels of this frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotResult {
    pub target: String,
    pub frame_id: String,
    pub width: i64,
    pub height: i64,
    pub source_width: i64,
    pub source_height: i64,
    pub scale: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn caption(frame: &CaptureResult) -> String {
    let size = format!("{}x{}", frame.width, frame.height);
    let scaled = frame.width != frame.source_width || frame.height != frame.source_height;
    let source = if scaled {
        format!(" (scaled from {}x{})", frame.source_width, frame.source_height)
    } else {
        String::new()
    };
    let artifact = frame
        .artifact_path
        .as_ref()
        .map(|p| format!(" -> {p}"))
        .unwrap_or_default();
    format!("screenshot {} {}{}{}", frame.target, size, source, artifact)
}

/// `capture` under the snapshot's caps; the engine owns the byte budget and the coordinate frame.
pub async fn capture_screenshot(
    scope: &RunScope,
    target: &str,
    options: ScreenshotOptions,
) -> Result<ScreenshotResult, DesktopError> {
    let snapshot = &scope.context.snapshot;
    let caps = json!({
        "maxWidth": snapshot.capture_max_width,
        "maxHeight": snapshot.capture_max_height,
        "maxBytes": snapshot.capture_max_bytes,
    });

    let res = (scope.call)("capture", json!({ "target": target, "caps": caps })).await?;
    let frame: CaptureResult = expect_result("capture", res)?;

    let path = frame.artifact_path.clone();
    if let Some(artifact_path) = &path {
        let mut screenshots = scope.context.screenshots.lock().await;
        screenshots.push(ComputerScreenshot {
            path: artifact_path.clone(),
            width: frame.width.max(0) as u32,
            height: frame.height.max(0) as u32,
            source_width: Some(frame.source_width.max(0) as u32),
            source_height: Some(frame.source_height.max(0) as u32),
            target: frame.target.clone(),
        });
    }

    if !options.silent {
        scope.context.output.text(caption(&frame)).await;
        if frame.mode != "artifact-only" && frame.data.is_some() && frame.mime_type.is_some() {
            scope
                .context
                .output
                .image(
                    frame.data.as_deref().unwrap_or_default(),
                    frame.mime_type.as_deref().unwrap_or_default(),
                )
                .await;
        } else {
            scope
                .context
                .output
                .text(
                    frame
                        .note
                        .as_deref()
                        .unwrap_or("screenshot is available only as the artifact above"),
                )
                .await;
        }
    }

    Ok(ScreenshotResult {
        target: frame.target,
        frame_id: frame.frame_id,
        width: frame.width,
        height: frame.height,
        source_width: frame.source_width,
        source_height: frame.source_height,
        scale: frame.scale,
        path,
    })
}
