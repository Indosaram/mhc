//! Port of `packages/machine-ops/src/process/holder-link.ts`.

use crate::types::OpFuture;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

/// Environment entry update when spawning a held terminal (`None` unsets an inherited variable).
pub type HolderEnv = BTreeMap<String, Option<String>>;

#[derive(Clone, Debug)]
pub struct HolderSpawnRequest {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HolderEnv,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldExit {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

pub trait HeldTerminal: Send + Sync {
    fn id(&self) -> &str;
    fn pid(&self) -> u32;
    /// Broadcast receiver of raw output chunks in order.
    fn subscribe_data(&self) -> broadcast::Receiver<Vec<u8>>;
    /// Watch receiver that becomes `Some(HeldExit)` once the terminal has exited.
    fn subscribe_exit(&self) -> watch::Receiver<Option<HeldExit>>;
    fn write(&self, bytes: &[u8]);
    fn resize(&self, cols: u16, rows: u16);
    fn signal(&self, signal: &str);
    /// Rendered terminal screen (visible grid plus up to `history` scrollback lines).
    fn screen(&self, history: Option<usize>) -> OpFuture<'_, String>;
}

pub trait HolderLink: Send + Sync {
    fn spawn(&self, req: HolderSpawnRequest) -> OpFuture<'_, Arc<dyn HeldTerminal>>;
    fn close(&self) -> OpFuture<'_, ()>;
}
