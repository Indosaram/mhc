//! Parent-forwarded child UI dialogs (`runners/child-dialogs/*`, `#6189 F2d`).

pub mod foreground_origins;
pub mod forwardable_dialog;
pub mod forwarder;
pub mod forwarding_ui_context;

pub use foreground_origins::{ForegroundDialogOrigins, shared_foreground_dialog_origins};
pub use forwardable_dialog::{
    CANCELLED_CHILD_DIALOG, ForwardableDialog, ForwardedAnswer, forwardable_dialog_of,
    forwarded_ui_response,
};
pub use forwarder::{
    ChildDialogForwarder, ChildDialogSignal, ChildUiAnswering, ForwardedChild,
    answer_child_ui_request,
};
pub use forwarding_ui_context::{DialogPromptUi, ForwardingDialogUi, create_forwarding_ui_context};
