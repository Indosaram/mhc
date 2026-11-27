//! Port of `packages/machine-ops/src/process/in-process-holder.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::process::holder_link::{HeldExit, HeldTerminal, HolderLink, HolderSpawnRequest};
use crate::process::pty_lazy::spawn_pty_session;
use crate::types::OpFuture;
use maho_pty::{PtySession, PtySessionOptions};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

const SCREEN_SCROLLBACK: usize = 1000;
const DATA_BROADCAST_CAPACITY: usize = 1024;

struct InProcessTerminal {
    id: String,
    pid: u32,
    session: Arc<Mutex<PtySession>>,
    screen: Arc<Mutex<vt100::Parser>>,
    data_tx: broadcast::Sender<Vec<u8>>,
    exit_tx: watch::Sender<Option<HeldExit>>,
}

impl InProcessTerminal {
    fn spawn(req: HolderSpawnRequest) -> Result<Arc<Self>, MachineOpError> {
        let mut opts = PtySessionOptions::new(req.command);
        for arg in req.args {
            opts = opts.arg(arg);
        }
        if let Some(cwd) = req.cwd {
            opts = opts.cwd(cwd);
        }
        for (key, maybe_value) in req.env {
            match maybe_value {
                Some(val) => opts = opts.env(key, val),
                None => opts = opts.env_remove(key),
            }
        }
        let cols = if req.cols == 0 { 80 } else { req.cols };
        let rows = if req.rows == 0 { 24 } else { req.rows };
        opts = opts.size(cols, rows);

        let screen = Arc::new(Mutex::new(vt100::Parser::new(
            rows,
            cols,
            SCREEN_SCROLLBACK,
        )));
        let (data_tx, _) = broadcast::channel(DATA_BROADCAST_CAPACITY);
        let (exit_tx, _) = watch::channel(None);

        let screen_for_chunk = Arc::clone(&screen);
        let data_tx_for_chunk = data_tx.clone();
        let on_chunk = Arc::new(move |chunk: &[u8]| {
            if let Ok(mut parser) = screen_for_chunk.lock() {
                parser.process(chunk);
            }
            let _ = data_tx_for_chunk.send(chunk.to_vec());
        });

        let mut session = spawn_pty_session(opts, on_chunk)
            .map_err(|err| MachineOpError::new(OpErrorCode::IoError, err.to_string()))?;
        let pid = session.pid().ok_or_else(|| {
            MachineOpError::new(
                OpErrorCode::IoError,
                "terminal session started without a pid",
            )
        })?;
        let wait_handle = session
            .wait_in_background()
            .map_err(|err| MachineOpError::new(OpErrorCode::IoError, err.to_string()))?;

        let exit_tx_for_wait = exit_tx.clone();
        std::thread::spawn(move || {
            let held_exit = match wait_handle.join() {
                Ok(Ok(exit)) => HeldExit {
                    exit_code: exit.exit_code,
                    signal: if exit.cancelled && exit.exit_code.is_none() {
                        Some("SIGKILL".to_owned())
                    } else {
                        None
                    },
                },
                _ => HeldExit {
                    exit_code: None,
                    signal: None,
                },
            };
            let _ = exit_tx_for_wait.send(Some(held_exit));
        });

        Ok(Arc::new(Self {
            id: req.id,
            pid,
            session: Arc::new(Mutex::new(session)),
            screen,
            data_tx,
            exit_tx,
        }))
    }
}

impl HeldTerminal for InProcessTerminal {
    fn id(&self) -> &str {
        &self.id
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn subscribe_data(&self) -> broadcast::Receiver<Vec<u8>> {
        self.data_tx.subscribe()
    }

    fn subscribe_exit(&self) -> watch::Receiver<Option<HeldExit>> {
        self.exit_tx.subscribe()
    }

    fn write(&self, bytes: &[u8]) {
        if let Ok(mut session) = self.session.lock() {
            let _ = session.write(bytes);
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        if let Ok(session) = self.session.lock() {
            let _ = session.resize(cols, rows);
        }
        if let Ok(mut parser) = self.screen.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    fn signal(&self, signal: &str) {
        if let Ok(mut session) = self.session.lock() {
            if signal == "SIGKILL" {
                let _ = session.kill();
            } else {
                let _ = session.signal(signal);
            }
        }
    }

    fn screen(&self, history: Option<usize>) -> OpFuture<'_, String> {
        let history = history.unwrap_or(0);
        Box::pin(async move {
            let mut parser = self
                .screen
                .lock()
                .map_err(|_| MachineOpError::new(OpErrorCode::IoError, "screen lock poisoned"))?;
            let screen = parser.screen_mut();
            let (_rows, cols) = screen.size();
            let visible: Vec<String> = screen.rows(0, cols).collect();
            let mut lines = Vec::new();
            if history > 0 {
                screen.set_scrollback(usize::MAX);
                let total_scrollback = screen.scrollback();
                let take_count = total_scrollback.min(history);
                for offset in (1..=take_count).rev() {
                    screen.set_scrollback(offset);
                    if let Some(row) = screen.rows(0, cols).next() {
                        lines.push(row);
                    }
                }
                screen.set_scrollback(0);
            }
            lines.extend(visible);
            let joined = lines.join("\n");
            Ok(joined.trim_end().to_owned())
        })
    }
}

/// Holder link whose terminals live in this process over `maho-pty`.
pub struct InProcessHolderLink {
    live: Arc<Mutex<HashMap<String, Arc<InProcessTerminal>>>>,
}

impl InProcessHolderLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

pub fn create_in_process_holder_link() -> Arc<dyn HolderLink> {
    InProcessHolderLink::new()
}

impl HolderLink for InProcessHolderLink {
    fn spawn(&self, req: HolderSpawnRequest) -> OpFuture<'_, Arc<dyn HeldTerminal>> {
        Box::pin(async move {
            let terminal = InProcessTerminal::spawn(req)?;
            if let Ok(mut live) = self.live.lock() {
                live.insert(terminal.id.clone(), Arc::clone(&terminal));
            }
            let id = terminal.id.clone();
            let mut exit_rx = terminal.subscribe_exit();
            let live_map = Arc::clone(&self.live);
            tokio::spawn(async move {
                while exit_rx.borrow().is_none() {
                    if exit_rx.changed().await.is_err() {
                        break;
                    }
                }
                if let Ok(mut live) = live_map.lock() {
                    live.remove(&id);
                }
            });
            Ok(terminal as Arc<dyn HeldTerminal>)
        })
    }

    fn close(&self) -> OpFuture<'_, ()> {
        Box::pin(async move {
            let terminals: Vec<Arc<InProcessTerminal>> = {
                let mut live = self
                    .live
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                live.drain().map(|(_, term)| term).collect()
            };
            for terminal in terminals {
                if terminal.exit_tx.borrow().is_none() {
                    terminal.signal("SIGKILL");
                }
            }
            Ok(())
        })
    }
}
