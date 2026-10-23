//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "native_video.rs"]
mod native_video;
#[path = "video.rs"]
mod video;
