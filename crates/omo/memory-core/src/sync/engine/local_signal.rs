//! Single-threaded wake primitives the engine's scheduler is built from (no extra runtime
//! features): a one-shot signal and a generation counter tasks can wait on.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct SignalState {
    fired: bool,
    waker: Option<Waker>,
}

/// Fires once; awaiting it completes after `fire`.
#[derive(Clone, Default)]
pub struct Signal(Rc<RefCell<SignalState>>);

impl Signal {
    pub fn fire(&self) {
        let waker = {
            let mut state = self.0.borrow_mut();
            state.fired = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub fn wait(&self) -> SignalWait {
        SignalWait(self.0.clone())
    }
}

pub struct SignalWait(Rc<RefCell<SignalState>>);

impl Future for SignalWait {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.0.borrow_mut();
        if state.fired {
            Poll::Ready(())
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[derive(Default)]
struct GenerationState {
    generation: u64,
    wakers: Vec<Waker>,
}

/// A counter whose bumps wake every waiter.
#[derive(Clone, Default)]
pub struct Generation(Rc<RefCell<GenerationState>>);

impl Generation {
    pub fn current(&self) -> u64 {
        self.0.borrow().generation
    }

    pub fn bump(&self) {
        let wakers = {
            let mut state = self.0.borrow_mut();
            state.generation += 1;
            std::mem::take(&mut state.wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// Completes once the counter differs from `seen`.
    pub fn changed(&self, seen: u64) -> GenerationWait {
        GenerationWait {
            state: self.0.clone(),
            seen,
        }
    }
}

pub struct GenerationWait {
    state: Rc<RefCell<GenerationState>>,
    seen: u64,
}

impl Future for GenerationWait {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.state.borrow_mut();
        if state.generation != self.seen {
            Poll::Ready(())
        } else {
            state.wakers.push(cx.waker().clone());
            Poll::Pending
        }
    }
}
