//! Soul path identification and out-of-band edit notice consumption.

pub mod paths;
pub mod watermark;

pub use paths::{
    MEMORY_SOUL_EDIT_RESULT_TOKEN, SOUL_EDIT_RESULT_LINE, SOUL_PATHS, touches_soul_path,
};
pub use watermark::{
    ConsumeSoulNoticeOptions, SOUL_NOTICE_WATERMARK_FILENAME, SoulNotice, WatermarkError,
    consume_soul_notice_delta,
};
