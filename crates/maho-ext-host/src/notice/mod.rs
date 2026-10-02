pub mod spec;
pub mod box_;
pub mod adapters;
pub use adapters::{notice_entry_renderer, notice_message_renderer};
pub use box_::build_notice_box;
pub use spec::{NoticeLine, NoticeSpec, NoticeTone};
