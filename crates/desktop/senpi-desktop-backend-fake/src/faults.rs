use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;
use senpi_desktop_core::error::{CoreResult, DesktopError, ErrorCode};

use crate::method::{AxWrite, FakeMethod};

/// Shared per-method failure queues. Each queued error fails exactly one call
/// of its method, in FIFO order; clones share the queues.
#[derive(Debug, Clone, Default)]
pub struct Faults {
    queues: Arc<Mutex<BTreeMap<FakeMethod, VecDeque<Fault>>>>,
}

#[derive(Debug)]
enum Fault {
    Fail(DesktopError),
    CannotComplete,
}

impl Faults {
    /// Makes the next call of `method` fail with `code`.
    pub fn fail_next(&self, method: FakeMethod, code: ErrorCode) {
        self.queues
            .lock()
            .entry(method)
            .or_default()
            .push_back(Fault::Fail(DesktopError::new(
                code,
                format!("injected {method:?} failure"),
            )));
    }

    /// Leaves the next `write` unanswered: its side effect lands, then the
    /// call reports `AxUnconfirmed`.
    pub fn cannot_complete_next(&self, write: AxWrite) {
        self.queues
            .lock()
            .entry(write.method())
            .or_default()
            .push_back(Fault::CannotComplete);
    }

    pub(crate) fn take(&self, method: FakeMethod) -> Option<DesktopError> {
        self.next(method).err()
    }

    /// [`Self::take`] for an AX write, which a `CannotComplete` fault lets
    /// run unanswered.
    pub(crate) fn take_write(&self, write: AxWrite) -> CoreResult<Reply> {
        self.next(write.method())
    }

    fn next(&self, method: FakeMethod) -> CoreResult<Reply> {
        let fault = self
            .queues
            .lock()
            .get_mut(&method)
            .and_then(VecDeque::pop_front);
        match fault {
            Some(Fault::Fail(error)) => Err(error),
            Some(Fault::CannotComplete) => Ok(Reply::Unanswered),
            None => Ok(Reply::Answered),
        }
    }
}

/// Whether the fake app answered an AX write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    Answered,
    Unanswered,
}

impl Reply {
    /// The call's result once the write has landed.
    pub(crate) fn confirm(self, request: &str) -> CoreResult<()> {
        match self {
            Self::Answered => Ok(()),
            Self::Unanswered => Err(DesktopError::ax_unconfirmed(format!(
                "{request} was requested, but its outcome could not be confirmed: the fake app did \
                 not reply (injected ax_cannot_complete). It may already have taken effect: observe \
                 the window before retrying"
            ))),
        }
    }
}
