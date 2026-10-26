//! Native Rust port of `src/run/handles.ts`.
//!
//! Input targets, window handles, and accessibility element handles.

use std::collections::HashMap;
use senpi_desktop_protocol::{ELEMENT_METHODS, WINDOW_METHODS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::context::{guard_run, DesktopRunError, MethodTiers, RunScope};
use crate::engine_results::{
    expect_ax_node, expect_ax_nodes, expect_optional_ax_node, expect_result, AxNode, DesktopWindow,
    TextResult,
};
use crate::screenshot::{capture_screenshot, ScreenshotOptions, ScreenshotResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeliveryOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClickOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScrollOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    #[serde(default)]
    pub dx: i64,
    #[serde(default)]
    pub dy: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AxOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AxQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

fn pointer_options(options: ClickOptions) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(button) = options.button {
        map.insert("button".into(), json!(button));
    }
    if let Some(count) = options.count {
        map.insert("count".into(), json!(count));
    }
    if let Some(modifiers) = options.modifiers {
        map.insert("modifiers".into(), json!(modifiers));
    }
    if let Some(delivery) = options.delivery {
        map.insert("deliveryMode".into(), json!(delivery));
    }
    Value::Object(map)
}

fn chord_keys(chord: &str) -> Vec<String> {
    chord
        .split('+')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Input helpers aimed at one engine target (`desktop` or a window id).
pub struct InputTarget {
    pub scope: RunScope,
    pub target: String,
    pub tiers: MethodTiers,
}

impl InputTarget {
    pub fn new(scope: RunScope, target: String, tiers: MethodTiers) -> Self {
        Self {
            scope,
            target,
            tiers,
        }
    }

    pub async fn screenshot(
        &self,
        options: ScreenshotOptions,
    ) -> Result<ScreenshotResult, DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "screenshot").await?;
        capture_screenshot(&self.scope, &self.target, options)
            .await
            .map_err(DesktopRunError::Engine)
    }

    pub async fn click(&self, x: i64, y: i64, options: ClickOptions) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "click").await?;
        let opts = pointer_options(options);
        (self.scope.call)(
            "click",
            json!({
                "target": self.target,
                "x": x,
                "y": y,
                "opts": opts,
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn double_click(
        &self,
        x: i64,
        y: i64,
        options: ClickOptions,
    ) -> Result<(), DesktopRunError> {
        let mut opts = options;
        opts.count = Some(2);
        self.click(x, y, opts).await
    }

    pub async fn move_mouse(&self, x: i64, y: i64) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "move").await?;
        (self.scope.call)(
            "moveMouse",
            json!({
                "target": self.target,
                "x": x,
                "y": y,
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn drag(
        &self,
        points: &[(i64, i64)],
        options: ClickOptions,
    ) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "drag").await?;
        let path: Vec<Value> = points
            .iter()
            .map(|(x, y)| json!({ "x": x, "y": y }))
            .collect();
        let opts = pointer_options(options);
        (self.scope.call)(
            "drag",
            json!({
                "target": self.target,
                "path": path,
                "opts": opts,
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn scroll(
        &self,
        x: i64,
        y: i64,
        options: ScrollOptions,
    ) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "scroll").await?;
        let mut opts_map = serde_json::Map::new();
        if let Some(delivery) = options.delivery {
            opts_map.insert("deliveryMode".into(), json!(delivery));
        }
        (self.scope.call)(
            "scroll",
            json!({
                "target": self.target,
                "x": x,
                "y": y,
                "dx": options.dx,
                "dy": options.dy,
                "opts": Value::Object(opts_map),
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn type_text(
        &self,
        text: &str,
        options: DeliveryOptions,
    ) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "type").await?;
        let mut opts_map = serde_json::Map::new();
        if let Some(delivery) = options.delivery {
            opts_map.insert("deliveryMode".into(), json!(delivery));
        }
        (self.scope.call)(
            "typeText",
            json!({
                "target": self.target,
                "text": text,
                "opts": Value::Object(opts_map),
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn press(
        &self,
        chord: &str,
        options: DeliveryOptions,
    ) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, self.tiers, "press").await?;
        let keys = chord_keys(chord);
        let mut opts_map = serde_json::Map::new();
        if let Some(delivery) = options.delivery {
            opts_map.insert("deliveryMode".into(), json!(delivery));
        }
        (self.scope.call)(
            "keyChord",
            json!({
                "target": self.target,
                "keys": keys,
                "opts": Value::Object(opts_map),
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }
}

/// A window resolved by `desktop.window()`; identity fields are a snapshot, calls go by id.
pub struct WindowHandle {
    pub input: InputTarget,
    pub id: String,
    pub app: String,
    pub title: String,
    pub pid: Option<i64>,
    pub bounds: Bounds,
    pub focused: bool,
}

impl WindowHandle {
    pub fn new(scope: RunScope, window: DesktopWindow) -> Self {
        let input = InputTarget::new(scope, window.id.clone(), WINDOW_METHODS);
        Self {
            input,
            id: window.id,
            app: window.app,
            title: window.title,
            pid: window.pid,
            bounds: Bounds {
                x: window.x,
                y: window.y,
                width: window.width,
                height: window.height,
            },
            focused: window.focused,
        }
    }

    pub async fn screenshot(
        &self,
        options: ScreenshotOptions,
    ) -> Result<ScreenshotResult, DesktopRunError> {
        self.input.screenshot(options).await
    }

    pub async fn click(&self, x: i64, y: i64, options: ClickOptions) -> Result<(), DesktopRunError> {
        self.input.click(x, y, options).await
    }

    pub async fn double_click(
        &self,
        x: i64,
        y: i64,
        options: ClickOptions,
    ) -> Result<(), DesktopRunError> {
        self.input.double_click(x, y, options).await
    }

    pub async fn raise(&self) -> Result<(), DesktopRunError> {
        guard_run(&self.input.scope.context, WINDOW_METHODS, "raise").await?;
        (self.input.scope.call)("raiseWindow", json!({ "windowId": self.id }))
            .await
            .map(|_| ())
            .map_err(DesktopRunError::Engine)
    }

    pub async fn ax(&self, options: AxOptions) -> Result<String, DesktopRunError> {
        guard_run(&self.input.scope.context, WINDOW_METHODS, "ax").await?;
        let res = (self.input.scope.call)(
            "ax.snapshot",
            json!({ "target": self.id, "opts": options }),
        )
        .await
        .map_err(DesktopRunError::Engine)?;
        let text: TextResult =
            expect_result("ax.snapshot", res).map_err(DesktopRunError::Engine)?;
        Ok(text.text)
    }

    pub async fn find(&self, query: AxQuery) -> Result<Vec<ElementHandle>, DesktopRunError> {
        guard_run(&self.input.scope.context, WINDOW_METHODS, "find").await?;
        let res = (self.input.scope.call)(
            "ax.query",
            json!({ "target": self.id, "query": query }),
        )
        .await
        .map_err(DesktopRunError::Engine)?;
        let nodes = expect_ax_nodes("ax.query", res)
            .map_err(DesktopRunError::Engine)?;
        Ok(nodes
            .into_iter()
            .map(|n| ElementHandle::new(self.input.scope.clone(), n))
            .collect())
    }

    pub async fn ref_elem(&self, ref_id: &str) -> Result<ElementHandle, DesktopRunError> {
        guard_run(&self.input.scope.context, WINDOW_METHODS, "ref").await?;
        resolve_element(&self.input.scope, ref_id).await
    }
}

pub async fn resolve_element(scope: &RunScope, ref_id: &str) -> Result<ElementHandle, DesktopRunError> {
    let res = (scope.call)("ax.node", json!({ "ref": ref_id }))
        .await
        .map_err(DesktopRunError::Engine)?;
    let node = expect_ax_node("ax.node", res)
        .map_err(DesktopRunError::Engine)?;
    Ok(ElementHandle::new(scope.clone(), node))
}

/// An accessibility element; identity fields are snapshot, calls re-resolve in the engine.
pub struct ElementHandle {
    pub scope: RunScope,
    pub ref_id: String,
    pub role: String,
    pub native_role: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub child_count: usize,
}

impl ElementHandle {
    pub fn new(scope: RunScope, node: AxNode) -> Self {
        Self {
            scope,
            ref_id: node.ref_id,
            role: node.role,
            native_role: node.native_role,
            title: node.title,
            description: node.description,
            enabled: node.enabled,
            focused: node.focused,
            child_count: node.child_count,
        }
    }

    async fn node(&self, method: &str) -> Result<AxNode, DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, method).await?;
        let res = (self.scope.call)("ax.node", json!({ "ref": self.ref_id }))
            .await
            .map_err(DesktopRunError::Engine)?;
        expect_ax_node("ax.node", res).map_err(DesktopRunError::Engine)
    }

    pub async fn value(&self) -> Result<Option<String>, DesktopRunError> {
        let n = self.node("value").await?;
        Ok(n.value)
    }

    pub async fn set_value(&self, value: &str) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "setValue").await?;
        (self.scope.call)(
            "ax.setValue",
            json!({ "ref": self.ref_id, "value": value }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn bounds(&self) -> Result<Option<Bounds>, DesktopRunError> {
        let n = self.node("bounds").await?;
        if let (Some(x), Some(y), Some(width), Some(height)) = (n.x, n.y, n.width, n.height) {
            Ok(Some(Bounds { x, y, width, height }))
        } else {
            Ok(None)
        }
    }

    pub async fn attributes(&self) -> Result<HashMap<String, String>, DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "attributes").await?;
        let res = (self.scope.call)("ax.attributes", json!({ "ref": self.ref_id }))
            .await
            .map_err(DesktopRunError::Engine)?;
        let pairs: Vec<(String, String)> =
            expect_result("ax.attributes", res).map_err(DesktopRunError::Engine)?;
        Ok(pairs.into_iter().collect())
    }

    pub async fn actions(&self) -> Result<Vec<String>, DesktopRunError> {
        let n = self.node("actions").await?;
        Ok(n.actions.unwrap_or_default())
    }

    pub async fn perform(&self, action: &str) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "perform").await?;
        (self.scope.call)(
            "ax.perform",
            json!({ "ref": self.ref_id, "action": action }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn press(&self) -> Result<(), DesktopRunError> {
        self.perform("press").await
    }

    pub async fn click(&self, options: DeliveryOptions) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "click").await?;
        let mut opts_map = serde_json::Map::new();
        if let Some(delivery) = options.delivery {
            opts_map.insert("deliveryMode".into(), json!(delivery));
        }
        (self.scope.call)(
            "ax.click",
            json!({
                "ref": self.ref_id,
                "opts": Value::Object(opts_map),
            }),
        )
        .await
        .map(|_| ())
        .map_err(DesktopRunError::Engine)
    }

    pub async fn focus(&self) -> Result<(), DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "focus").await?;
        (self.scope.call)("ax.focus", json!({ "ref": self.ref_id }))
            .await
            .map(|_| ())
            .map_err(DesktopRunError::Engine)
    }

    pub async fn parent(&self) -> Result<Option<ElementHandle>, DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "parent").await?;
        let res = (self.scope.call)("ax.parent", json!({ "ref": self.ref_id }))
            .await
            .map_err(DesktopRunError::Engine)?;
        let node = expect_optional_ax_node("ax.parent", res)
            .map_err(DesktopRunError::Engine)?;
        Ok(node.map(|n| ElementHandle::new(self.scope.clone(), n)))
    }

    pub async fn children(&self) -> Result<Vec<ElementHandle>, DesktopRunError> {
        guard_run(&self.scope.context, ELEMENT_METHODS, "children").await?;
        let res = (self.scope.call)("ax.children", json!({ "ref": self.ref_id }))
            .await
            .map_err(DesktopRunError::Engine)?;
        let nodes = expect_ax_nodes("ax.children", res)
            .map_err(DesktopRunError::Engine)?;
        Ok(nodes
            .into_iter()
            .map(|n| ElementHandle::new(self.scope.clone(), n))
            .collect())
    }
}
