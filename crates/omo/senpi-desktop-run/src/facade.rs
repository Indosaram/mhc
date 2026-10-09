//! Native Rust port of `src/run/facade.ts`.
//!
//! Root `desktop` facade providing capabilities, window listing, and input delegation.

use senpi_desktop_protocol::{DesktopCapabilities, DESKTOP_METHODS};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::context::{guard_run, ComputerRunError, DesktopRunError, RunScope};
use crate::engine_results::{
    expect_optional_ax_node, expect_result, DesktopDisplay, DesktopWindow,
    TextResult,
};
use crate::handles::{
    resolve_element, ClickOptions, DeliveryOptions, ElementHandle, InputTarget, ScrollOptions,
    WindowHandle,
};
use crate::screenshot::{ScreenshotOptions, ScreenshotResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WindowFilter {
    /// Case-insensitive substring of the owning app name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Case-insensitive substring of the window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

pub fn matches_filter(window: &DesktopWindow, filter: &WindowFilter) -> bool {
    let app_match = filter
        .app
        .as_ref()
        .map(|a| window.app.to_lowercase().contains(&a.to_lowercase()))
        .unwrap_or(true);
    let title_match = filter
        .title
        .as_ref()
        .map(|t| window.title.to_lowercase().contains(&t.to_lowercase()))
        .unwrap_or(true);
    app_match && title_match
}

#[derive(Debug, Clone)]
pub enum WindowSelector {
    Id(String),
    Filter(WindowFilter),
}

/// The `desktop` global facade of a computer run.
pub struct DesktopFacade {
    pub root: InputTarget,
}

impl DesktopFacade {
    pub fn new(scope: RunScope) -> Self {
        let root = InputTarget::new(scope, "desktop".to_string(), DESKTOP_METHODS);
        Self { root }
    }

    pub fn scope(&self) -> &RunScope {
        &self.root.scope
    }

    pub async fn capabilities(&self) -> Result<DesktopCapabilities, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "capabilities").await?;
        let res = (self.root.scope.call)("capabilities", json!({}))
            .await
            .map_err(DesktopRunError::Engine)?;
        expect_result("capabilities", res).map_err(DesktopRunError::Engine)
    }

    pub async fn displays(&self) -> Result<Vec<DesktopDisplay>, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "displays").await?;
        let res = (self.root.scope.call)("displays", json!({}))
            .await
            .map_err(DesktopRunError::Engine)?;
        expect_result("displays", res).map_err(DesktopRunError::Engine)
    }

    pub async fn windows(
        &self,
        filter: Option<WindowFilter>,
    ) -> Result<Vec<DesktopWindow>, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "windows").await?;
        let res = (self.root.scope.call)("windows", json!({}))
            .await
            .map_err(DesktopRunError::Engine)?;
        let list: Vec<DesktopWindow> =
            expect_result("windows", res).map_err(DesktopRunError::Engine)?;
        if let Some(f) = filter {
            Ok(list.into_iter().filter(|w| matches_filter(w, &f)).collect())
        } else {
            Ok(list)
        }
    }

    pub async fn window(
        &self,
        selector: WindowSelector,
    ) -> Result<WindowHandle, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "window").await?;
        let all = self.windows(None).await?;
        let found: Vec<DesktopWindow> = match &selector {
            WindowSelector::Id(id) => all.into_iter().filter(|w| &w.id == id).collect(),
            WindowSelector::Filter(f) => all.into_iter().filter(|w| matches_filter(w, f)).collect(),
        };

        if found.is_empty() {
            let label = match &selector {
                WindowSelector::Id(id) => format!("\"{id}\""),
                WindowSelector::Filter(f) => serde_json::to_string(f).unwrap_or_default(),
            };
            return Err(ComputerRunError::window(format!("no window matches {label}")).into());
        }
        if found.len() > 1 {
            let candidates: Vec<String> = found
                .iter()
                .map(|w| format!("{} {} {:?}", w.id, w.app, w.title))
                .collect();
            let label = match &selector {
                WindowSelector::Id(id) => format!("\"{id}\""),
                WindowSelector::Filter(f) => serde_json::to_string(f).unwrap_or_default(),
            };
            return Err(ComputerRunError::window(format!(
                "multiple windows match {label}:\n{}",
                candidates.join("\n")
            )).into());
        }
        let only = found.into_iter().next().ok_or_else(|| ComputerRunError::window("no window found"))?;
        Ok(WindowHandle::new(self.root.scope.clone(), only))
    }

    pub async fn focused_window(&self) -> Result<Option<WindowHandle>, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "focusedWindow").await?;
        let all = self.windows(None).await?;
        let focused = all.into_iter().find(|w| w.focused);
        Ok(focused.map(|w| WindowHandle::new(self.root.scope.clone(), w)))
    }

    pub async fn screenshot(
        &self,
        options: ScreenshotOptions,
    ) -> Result<ScreenshotResult, DesktopRunError> {
        self.root.screenshot(options).await
    }

    pub async fn click(&self, x: i64, y: i64, options: ClickOptions) -> Result<(), DesktopRunError> {
        self.root.click(x, y, options).await
    }

    pub async fn double_click(
        &self,
        x: i64,
        y: i64,
        options: ClickOptions,
    ) -> Result<(), DesktopRunError> {
        self.root.double_click(x, y, options).await
    }

    pub async fn move_mouse(&self, x: i64, y: i64) -> Result<(), DesktopRunError> {
        self.root.move_mouse(x, y).await
    }

    pub async fn drag(
        &self,
        points: &[(i64, i64)],
        options: ClickOptions,
    ) -> Result<(), DesktopRunError> {
        self.root.drag(points, options).await
    }

    pub async fn scroll(
        &self,
        x: i64,
        y: i64,
        options: ScrollOptions,
    ) -> Result<(), DesktopRunError> {
        self.root.scroll(x, y, options).await
    }

    pub async fn type_text(
        &self,
        text: &str,
        options: DeliveryOptions,
    ) -> Result<(), DesktopRunError> {
        self.root.type_text(text, options).await
    }

    pub async fn press(
        &self,
        chord: &str,
        options: DeliveryOptions,
    ) -> Result<(), DesktopRunError> {
        self.root.press(chord, options).await
    }

    pub async fn element_at(
        &self,
        x: i64,
        y: i64,
    ) -> Result<Option<ElementHandle>, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "elementAt").await?;
        let res = (self.root.scope.call)(
            "ax.elementAt",
            json!({ "target": "desktop", "x": x, "y": y }),
        )
        .await
        .map_err(DesktopRunError::Engine)?;
        let node = expect_optional_ax_node("ax.elementAt", res)
            .map_err(DesktopRunError::Engine)?;
        Ok(node.map(|n| ElementHandle::new(self.root.scope.clone(), n)))
    }

    pub async fn focused_element(&self) -> Result<Option<ElementHandle>, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "focusedElement").await?;
        let res = (self.root.scope.call)("ax.focused", json!({}))
            .await
            .map_err(DesktopRunError::Engine)?;
        let node = expect_optional_ax_node("ax.focused", res)
            .map_err(DesktopRunError::Engine)?;
        Ok(node.map(|n| ElementHandle::new(self.root.scope.clone(), n)))
    }

    pub async fn ref_elem(&self, ref_id: &str) -> Result<ElementHandle, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "ref").await?;
        resolve_element(&self.root.scope, ref_id).await
    }

    pub async fn clipboard_read(&self) -> Result<String, DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "clipboard.read").await?;
        let res = (self.root.scope.call)("clipboard.read", json!({}))
            .await
            .map_err(DesktopRunError::Engine)?;
        let text: TextResult =
            expect_result("clipboard.read", res).map_err(DesktopRunError::Engine)?;
        Ok(text.text)
    }

    pub async fn clipboard_write(&self, text: &str) -> Result<(), DesktopRunError> {
        guard_run(&self.root.scope.context, DESKTOP_METHODS, "clipboard.write").await?;
        (self.root.scope.call)("clipboard.write", json!({ "text": text }))
            .await
            .map(|_| ())
            .map_err(DesktopRunError::Engine)
    }
}
