//! senpi `modes/rpc/legacy-view-cursor.ts`: a legacy view frozen at one cut and read one frame at a
//! time by a peer's baseline barrier (R3.C6.4). The reader retains no array of generated frames;
//! `release` ends the read on every path and is idempotent.

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};

/// A legacy view frozen at one cut and read one frame at a time by a peer's baseline barrier.
#[async_trait]
pub trait LegacyViewCursor: Send {
    /// True when the frozen view has no frame at all: an attach installs no barrier and releases it
    /// at once.
    fn empty(&mut self) -> bool;

    /// Reads the next serialized JSONL frame, or `None` once exhausted or released.
    async fn next(&mut self) -> Option<String>;

    /// Ends the read on every path and runs the owner's release callback at most once.
    fn release(&mut self);
}

/// The frames of one view: how many, and each one derived on demand from the frozen source.
pub struct FrozenFrames {
    pub count: usize,
    pub frame: Box<dyn Fn(usize) -> String + Send + Sync>,
}

/// A cursor over frames the owner releases once. `source` runs on the first read (`next` or
/// `empty`): an owner that fixes the view at the freeze passes the frames it already built, one
/// that fixes it at the read passes a thunk.
pub struct FrozenFramesCursor {
    index: usize,
    released: bool,
    frames: Option<FrozenFrames>,
    source: Option<Box<dyn FnOnce() -> FrozenFrames + Send>>,
    on_release: Option<Box<dyn FnOnce() + Send>>,
}

impl FrozenFramesCursor {
    pub fn new(
        source: impl FnOnce() -> FrozenFrames + Send + 'static,
        on_release: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            index: 0,
            released: false,
            frames: None,
            source: Some(Box::new(source)),
            on_release: Some(Box::new(on_release)),
        }
    }

    fn read(&mut self) -> Option<&FrozenFrames> {
        if self.frames.is_none()
            && let Some(source) = self.source.take()
        {
            self.frames = Some(source());
        }
        self.frames.as_ref()
    }
}

#[async_trait]
impl LegacyViewCursor for FrozenFramesCursor {
    fn empty(&mut self) -> bool {
        self.read().is_none_or(|frames| frames.count == 0)
    }

    async fn next(&mut self) -> Option<String> {
        if self.released {
            return None;
        }
        let index = self.index;
        let frame = {
            let frames = self.read()?;
            if index >= frames.count {
                return None;
            }
            (frames.frame)(index)
        };
        self.index += 1;
        Some(frame)
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.source = None;
        self.frames = None;
        if let Some(on_release) = self.on_release.take() {
            on_release();
        }
    }
}

pub type FrameStream = BoxStream<'static, String>;

/// A cursor whose frames are produced one at a time by an async stream over a frozen source (the
/// compact current-state view). `empty` is decided synchronously by the owner from the frozen
/// roots, so an attach with nothing to deliver installs no barrier; the stream starts on the first
/// read.
pub struct GeneratedFramesCursor {
    empty: bool,
    frames: Option<FrameStream>,
    released: bool,
    source: Option<Box<dyn FnOnce() -> FrameStream + Send>>,
    on_release: Option<Box<dyn FnOnce() + Send>>,
}

impl GeneratedFramesCursor {
    pub fn new(
        empty: bool,
        source: impl FnOnce() -> FrameStream + Send + 'static,
        on_release: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            empty,
            frames: None,
            released: false,
            source: Some(Box::new(source)),
            on_release: Some(Box::new(on_release)),
        }
    }
}

#[async_trait]
impl LegacyViewCursor for GeneratedFramesCursor {
    fn empty(&mut self) -> bool {
        self.empty
    }

    async fn next(&mut self) -> Option<String> {
        if self.released {
            return None;
        }
        if self.frames.is_none()
            && let Some(source) = self.source.take()
        {
            self.frames = Some(source());
        }
        let frames = self.frames.as_mut()?;
        frames.next().await
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.frames = None;
        self.source = None;
        if let Some(on_release) = self.on_release.take() {
            on_release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn frozen_frames_cursor_reads_lazily_and_releases_once() {
        let built = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let built_clone = Arc::clone(&built);
        let released_clone = Arc::clone(&released);
        let mut cursor = FrozenFramesCursor::new(
            move || {
                built_clone.fetch_add(1, Ordering::SeqCst);
                FrozenFrames {
                    count: 2,
                    frame: Box::new(|i| format!("frame-{i}\n")),
                }
            },
            move || {
                released_clone.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(built.load(Ordering::SeqCst), 0);
        assert!(!cursor.empty());
        assert_eq!(built.load(Ordering::SeqCst), 1);
        assert_eq!(cursor.next().await.as_deref(), Some("frame-0\n"));
        assert_eq!(cursor.next().await.as_deref(), Some("frame-1\n"));
        assert_eq!(cursor.next().await, None);
        cursor.release();
        cursor.release();
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(cursor.next().await, None);
    }

    #[tokio::test]
    async fn generated_frames_cursor_reports_empty_without_starting_stream() {
        let started = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let started_clone = Arc::clone(&started);
        let released_clone = Arc::clone(&released);
        let mut cursor = GeneratedFramesCursor::new(
            true,
            move || {
                started_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(stream::iter(Vec::<String>::new()))
            },
            move || {
                released_clone.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(cursor.empty());
        assert_eq!(started.load(Ordering::SeqCst), 0);
        cursor.release();
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(cursor.next().await, None);
    }
}
