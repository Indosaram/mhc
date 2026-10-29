//! Failed initial input never gives up ownership of eventual cleanup.
use super::handle::HostSessionHandle;
use crate::host::HostError;
use crate::manager::child_handle::ManagedChildHandle;
use std::sync::{Arc, Mutex};
#[derive(Default)]
pub struct HostSessionOpener {
    teardowns: Mutex<Vec<std::thread::JoinHandle<Result<(), HostError>>>>,
}
pub fn create_host_session_opener() -> HostSessionOpener {
    HostSessionOpener::default()
}
impl HostSessionOpener {
    pub fn start_initial_prompt(
        &self,
        handle: Arc<HostSessionHandle>,
        text: &str,
    ) -> Result<Arc<HostSessionHandle>, HostError> {
        match handle.start_initial_prompt(text) {
            Ok(()) => Ok(handle),
            Err(error) => {
                self.teardowns
                    .lock()
                    .expect("hosted child state lock poisoned")
                    .push(std::thread::spawn(move || handle.dispose()));
                Err(error)
            }
        }
    }
    pub fn settle_teardowns(&self) -> Result<(), HostError> {
        let handles = std::mem::take(&mut *self.teardowns.lock().expect("hosted child state lock poisoned"));
        let mut first = None;
        for handle in handles {
            let result = handle
                .join()
                .map_err(|_| HostError {
                    message: "host teardown panicked".into(),
                })
                .and_then(|result| result);
            if let Err(error) = result
                && first.is_none() {
                    first = Some(error);
                }
        }
        first.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{handle_port::*, open_session::OpenedHostSession};
    use super::*;
    use crate::manager::child_handle::{ManagedChildListener, Unsubscribe};
    struct Port {
        entered: std::sync::mpsc::SyncSender<()>,
        released: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl HostSessionPort for Port {
        fn send(&self, _: serde_json::Value) -> Result<(), HostError> {
            Err(HostError {
                message: "prompt refused".into(),
            })
        }
        fn get_state(&self) -> Result<serde_json::Value, HostError> {
            Ok(serde_json::json!({}))
        }
        fn on_event(&self, _: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn close(&self) -> Result<(), HostError> {
            Ok(())
        }
        fn detach(&self) -> Result<(), HostError> {
            self.entered.send(()).unwrap();
            self.released
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            Ok(())
        }
    }
    #[test]
    fn rejected_prompt_returns_before_detach_and_settlement_joins_cleanup() {
        let (entered, rx) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let port = Arc::new(Port {
            entered,
            released: Mutex::new(released),
        });
        let opened = OpenedHostSession {
            session: HostSessionIdentity {
                routing_id: "route".into(),
                session_path: "path".into(),
                instance_id: "host".into(),
            },
            attached: false,
            durable_session_id: None,
            reported_model: None,
        };
        let handle = super::super::handle::create_host_session_handle(
            "task".into(),
            port,
            opened,
            Arc::new(|| crate::runners::RunnerOutcome::Cancelled),
        );
        let opener = create_host_session_opener();
        assert!(opener.start_initial_prompt(handle, "prompt").is_err());
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        release.send(()).unwrap();
        opener.settle_teardowns().unwrap();
    }
}
