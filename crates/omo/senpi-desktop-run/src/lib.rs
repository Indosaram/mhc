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
    guard_run, ComputerRunError, ComputerRunErrorReason, DesktopRunError, EngineCall, MethodTiers,
    RunContext, RunOutput, RunScope,
};
pub use engine_results::{
    expect_ax_node, expect_ax_nodes, expect_optional_ax_node, expect_result, AxNode, CaptureResult,
    DesktopDisplay, DesktopWindow, TextResult,
};
pub use facade::{matches_filter, DesktopFacade, WindowFilter, WindowSelector};
pub use handles::{
    resolve_element, AxOptions, AxQuery, Bounds, ClickOptions, DeliveryOptions, ElementHandle,
    InputTarget, ScrollOptions, WindowHandle,
};
pub use runtime::{run_computer_code, ComputerRunHost, ComputerRunRequest, ExecuteTool};
pub use screenshot::{capture_screenshot, ScreenshotOptions, ScreenshotResult};
pub use wait::{
    predicate_timeout, run_poll, run_sleep, WaitOptions, DEFAULT_INTERVAL_MS,
    DEFAULT_PREDICATE_TIMEOUT_MS, MIN_INTERVAL_MS, RUN_BUDGET_SLACK_MS,
};
