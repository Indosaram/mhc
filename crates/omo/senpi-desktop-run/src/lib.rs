//! Native Rust port of upstream `@oh-my-opencode/senpi-desktop-service/src/run`.
//!
//! Provides the desktop run facade, wait helpers, handle abstractions,
//! engine result decoders, screenshot captures, and JavaScript runtime evaluation.

pub mod context;
pub mod engine_results;
pub mod facade;
pub mod handles;
pub mod runtime;
pub mod screenshot;
pub mod wait;

pub use context::{
    ComputerRunError, ComputerRunErrorReason, DesktopRunError, EngineCall, MethodTiers, RunContext,
    RunOutput, RunScope, guard_run,
};
pub use engine_results::{
    AxNode, CaptureResult, DesktopDisplay, DesktopWindow, TextResult, expect_ax_node,
    expect_ax_nodes, expect_optional_ax_node, expect_result,
};
pub use facade::{DesktopFacade, WindowFilter, WindowSelector, matches_filter};
pub use handles::{
    AxOptions, AxQuery, Bounds, ClickOptions, DeliveryOptions, ElementHandle, InputTarget,
    ScrollOptions, WindowHandle, resolve_element,
};
pub use runtime::{ComputerRunHost, ComputerRunRequest, ExecuteTool, run_computer_code};
pub use screenshot::{ScreenshotOptions, ScreenshotResult, capture_screenshot};
pub use wait::{
    DEFAULT_INTERVAL_MS, DEFAULT_PREDICATE_TIMEOUT_MS, MIN_INTERVAL_MS, RUN_BUDGET_SLACK_MS,
    WaitOptions, predicate_timeout, run_poll, run_sleep,
};
