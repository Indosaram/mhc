//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-baseline-barrier.ts`.
//!
//! One peer's baseline attempt (R3.C6.4, S07a): the frames of a view frozen at a cut, written in
//! their FIFO position before every record queued behind them. `live` is rechecked after every
//! await and before every frame, so a stale attempt writes nothing further; `end` runs once on
//! every path and releases the frozen source.

use async_trait::async_trait;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A legacy view frozen at one cut and read one frame at a time (`LegacyViewCursor` in
/// `legacy-view-cursor.ts`).
#[async_trait]
pub trait LegacyViewCursor: Send {
    fn empty(&self) -> bool;
    async fn next(&mut self) -> Option<String>;
    fn release(&mut self);
}

/// One peer's baseline attempt.
pub struct BaselineBarrier {
    cancelled: AtomicBool,
    ended: AtomicBool,
    cursor: Box<dyn LegacyViewCursor>,
    live: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl BaselineBarrier {
    pub fn new(
        cursor: Box<dyn LegacyViewCursor>,
        live: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            cursor,
            live,
        }
    }

    pub fn active(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire)
            && !self.ended.load(Ordering::Acquire)
            && (self.live)()
    }

    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.end();
    }

    pub fn end(&mut self) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cursor.release();
    }

    /// Writes `head` (a recovery's gap line) once the cursor has fixed its view, then the frames
    /// in order; `write` resolves once the sink accepted the line.
    pub async fn deliver<'a, E, W>(&mut self, mut write: W, head: Option<&str>) -> Result<(), E>
    where
        W: FnMut(String) -> BoxFuture<'a, Result<(), E>>,
    {
        struct EndOnDrop<'b>(&'b mut BaselineBarrier);
        impl Drop for EndOnDrop<'_> {
            fn drop(&mut self) {
                self.0.end();
            }
        }
        let guard = EndOnDrop(self);
        if !guard.0.active() {
            return Ok(());
        }
        let mut line = guard.0.cursor.next().await;
        if let Some(head_line) = head
            && guard.0.active()
        {
            write(head_line.to_owned()).await?;
        }
        while let Some(current) = line {
            if !guard.0.active() {
                break;
            }
            write(current).await?;
            line = guard.0.cursor.next().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct VecCursor {
        frames: VecDeque<String>,
        released: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LegacyViewCursor for VecCursor {
        fn empty(&self) -> bool {
            self.frames.is_empty()
        }
        async fn next(&mut self) -> Option<String> {
            self.frames.pop_front()
        }
        fn release(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn deliver_writes_head_and_frames_and_releases_once() {
        let released = Arc::new(AtomicBool::new(false));
        let cursor = VecCursor {
            frames: VecDeque::from(["f1\n".into(), "f2\n".into()]),
            released: released.clone(),
        };
        let mut barrier = BaselineBarrier::new(Box::new(cursor), Arc::new(|| true));
        let written = Arc::new(Mutex::new(Vec::new()));
        let w = written.clone();
        barrier
            .deliver(
                move |line| {
                    let w = w.clone();
                    Box::pin(async move {
                        w.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(line);
                        Ok::<(), ()>(())
                    })
                },
                Some("gap\n"),
            )
            .await
            .expect("deliver");
        assert!(released.load(Ordering::SeqCst));
        assert_eq!(
            *written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["gap\n", "f1\n", "f2\n"]
        );
    }

    #[tokio::test]
    async fn deliver_stops_when_live_turns_false() {
        let released = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicBool::new(true));
        let cursor = VecCursor {
            frames: VecDeque::from(["f1\n".into(), "f2\n".into()]),
            released: released.clone(),
        };
        let live_flag = live.clone();
        let mut barrier = BaselineBarrier::new(
            Box::new(cursor),
            Arc::new(move || live_flag.load(Ordering::SeqCst)),
        );
        let written = Arc::new(Mutex::new(Vec::new()));
        let w = written.clone();
        let l = live.clone();
        barrier
            .deliver(
                move |line| {
                    let w = w.clone();
                    let l = l.clone();
                    Box::pin(async move {
                        w.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(line);
                        l.store(false, Ordering::SeqCst);
                        Ok::<(), ()>(())
                    })
                },
                None,
            )
            .await
            .expect("deliver");
        assert!(released.load(Ordering::SeqCst));
        assert_eq!(
            *written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["f1\n"]
        );
    }
}
