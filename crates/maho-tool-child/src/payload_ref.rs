//! Acknowledged payload references (`payload-ref.ts`). The writer owns a ref only until the receiver answers:
//! `ref_ack` transfers ownership (the writer forgets the file and never deletes it), `ref_nack` makes the writer
//! delete it. Settling the sending operation changes nothing; only generation teardown removes an unanswered ref.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::payload_ref_paths::{ref_file_path_of, teardown_ref_generation};
use crate::protocol::{PayloadRef, RefControl, RefNackReason, TOOL_CHILD_LIMITS};
use crate::protocol_guard::is_ref_id_str;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadRefErrorCode {
    RefBudgetExceeded,
    RefRejected,
    RefNotOwned,
}

impl PayloadRefErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RefBudgetExceeded => "ref_budget_exceeded",
            Self::RefRejected => "ref_rejected",
            Self::RefNotOwned => "ref_not_owned",
        }
    }
}

/// `PayloadRefError`: `message` is `"<code>: <detail>"`, like upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRefError {
    pub code: PayloadRefErrorCode,
    pub reason: Option<RefNackReason>,
    pub message: String,
}

impl PayloadRefError {
    pub fn new(code: PayloadRefErrorCode, message: &str, reason: Option<RefNackReason>) -> Self {
        Self {
            code,
            reason,
            message: format!("{}: {message}", code.as_str()),
        }
    }
}

impl fmt::Display for PayloadRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PayloadRefError {}

/// Everything a ref operation can throw upstream: its own `PayloadRefError`, or a raw fs / JSON error.
#[derive(Debug, thiserror::Error)]
pub enum PayloadRefFailure {
    #[error(transparent)]
    Ref(#[from] PayloadRefError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl PayloadRefFailure {
    /// The `PayloadRefError` code, when this is one.
    pub fn code(&self) -> Option<PayloadRefErrorCode> {
        match self {
            Self::Ref(error) => Some(error.code),
            _ => None,
        }
    }
}

/// Stand-in for the `AbortSignal` the TS API takes: cheap to clone, observed between file chunks.
#[derive(Debug, Clone, Default)]
pub struct AbortSignal(Arc<AtomicBool>);

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn is_aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

pub struct PayloadRefWriterOptions {
    pub root: PathBuf,
    pub generation: u64,
    pub budget: Option<u64>,
    pub now: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

impl PayloadRefWriterOptions {
    pub fn new(root: impl Into<PathBuf>, generation: u64) -> Self {
        Self {
            root: root.into(),
            generation,
            budget: None,
            now: None,
        }
    }
}

struct PendingRef {
    bytes: u64,
    created_at_ms: u64,
}

#[derive(Default)]
struct WriterState {
    /// Insertion-ordered like a JS `Map`.
    pending: Vec<(String, PendingRef)>,
    owned_bytes: u64,
}

/// Writer side of an acknowledged payload reference.
pub struct PayloadRefWriter {
    pub generation: u64,
    pub budget: u64,
    root: PathBuf,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    state: Mutex<WriterState>,
}

impl PayloadRefWriter {
    pub fn new(options: PayloadRefWriterOptions) -> Self {
        Self {
            generation: options.generation,
            budget: options.budget.unwrap_or(TOOL_CHILD_LIMITS.refs_budget),
            root: options.root,
            now: options.now.unwrap_or_else(|| Arc::new(system_now_ms)),
            state: Mutex::new(WriterState::default()),
        }
    }

    pub fn owned_bytes(&self) -> u64 {
        lock(&self.state).owned_bytes
    }

    pub async fn create(&self, payload: &[u8]) -> Result<PayloadRef, PayloadRefFailure> {
        let size = payload.len() as u64;
        {
            let mut state = lock(&self.state);
            if state.owned_bytes + size > self.budget {
                return Err(PayloadRefError::new(
                    PayloadRefErrorCode::RefBudgetExceeded,
                    &format!(
                        "{size} bytes on top of {} owned exceeds {}",
                        state.owned_bytes, self.budget
                    ),
                    None,
                )
                .into());
            }
            state.owned_bytes += size;
        }
        let mut id = [0u8; 12];
        if let Err(error) = getrandom::fill(&mut id) {
            lock(&self.state).owned_bytes -= size;
            return Err(io::Error::other(error.to_string()).into());
        }
        let payload_ref = PayloadRef {
            ref_id: id.iter().map(|byte| format!("{byte:02x}")).collect(),
            generation: self.generation,
            bytes: size,
            sha256: Sha256::digest(payload)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        let path = match ref_file_path_of(&self.root, &payload_ref) {
            Ok(path) => path,
            Err(error) => {
                lock(&self.state).owned_bytes -= size;
                return Err(error.into());
            }
        };
        if let Err(error) = write_new_ref_file(&path, payload).await {
            lock(&self.state).owned_bytes -= size;
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error.into());
        }
        let created_at_ms = (self.now)();
        lock(&self.state).pending.push((
            payload_ref.ref_id.clone(),
            PendingRef {
                bytes: size,
                created_at_ms,
            },
        ));
        Ok(payload_ref)
    }

    /// A `ref_ack` or `ref_nack` from the receiver; an answer for a ref this writer does not hold is ignored.
    pub async fn handle(&self, control: &RefControl) -> io::Result<()> {
        let ref_id = control.ref_id();
        {
            let mut state = lock(&self.state);
            let Some(index) = state.pending.iter().position(|(id, _)| id == ref_id) else {
                return Ok(());
            };
            let (_, pending) = state.pending.remove(index);
            state.owned_bytes -= pending.bytes;
        }
        if let RefControl::RefNack { .. } = control {
            let path =
                crate::payload_ref_paths::ref_file_path(&self.root, ref_id, self.generation)?;
            match tokio::fs::remove_file(path).await {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn pending_ids(&self) -> Vec<String> {
        lock(&self.state)
            .pending
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Refs still unanswered after `refAckMs`; reported for liveness handling, never deleted here.
    pub fn overdue(&self) -> Vec<String> {
        let deadline = (self.now)() as i128 - TOOL_CHILD_LIMITS.ref_ack_ms as i128;
        lock(&self.state)
            .pending
            .iter()
            .filter(|(_, pending)| pending.created_at_ms as i128 <= deadline)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub async fn teardown(&self) -> io::Result<()> {
        {
            let mut state = lock(&self.state);
            state.pending.clear();
            state.owned_bytes = 0;
        }
        teardown_ref_generation(&self.root, self.generation).await
    }
}

/// `mkdir(dirname, { recursive, mode: 0o700 })` then `writeFile(path, payload, { flag: "wx", mode: 0o600 })`.
async fn write_new_ref_file(path: &Path, payload: &[u8]) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent() {
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(parent).await?;
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    file.write_all(payload).await?;
    file.flush().await
}

pub struct PayloadRefReaderOptions {
    pub root: PathBuf,
    pub generation: u64,
    pub budget: Option<u64>,
    pub send: Arc<dyn Fn(RefControl) + Send + Sync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefAcceptance {
    Ok,
    Refused(RefNackReason),
}

impl RefAcceptance {
    pub fn is_ok(self) -> bool {
        self == Self::Ok
    }
}

fn is_missing_file(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// `Ok(None)` is upstream's `"cancelled"` result.
async fn hash_file(path: &Path, signal: Option<&AbortSignal>) -> io::Result<Option<String>> {
    let mut hash = Sha256::new();
    let mut file = tokio::fs::File::open(path).await?;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        if is_aborted(signal) {
            return Ok(None);
        }
        hash.update(&chunk[..read]);
    }
    Ok(Some(
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
}

#[derive(Default)]
struct ReaderState {
    owned: Vec<PayloadRef>,
    owned_bytes: u64,
}

/// Receiver side. `accept` verifies size and sha256 and answers `ref_ack` (taking ownership) or `ref_nack`.
/// The owner deletes the file when it consumes it; a dead owner's files go with its generation's teardown.
pub struct PayloadRefReader {
    root: PathBuf,
    generation: u64,
    budget: u64,
    send: Arc<dyn Fn(RefControl) + Send + Sync>,
    state: Mutex<ReaderState>,
}

impl PayloadRefReader {
    pub fn new(options: PayloadRefReaderOptions) -> Self {
        Self {
            root: options.root,
            generation: options.generation,
            budget: options.budget.unwrap_or(TOOL_CHILD_LIMITS.refs_budget),
            send: options.send,
            state: Mutex::new(ReaderState::default()),
        }
    }

    pub async fn accept(
        &self,
        payload_ref: &PayloadRef,
        signal: Option<&AbortSignal>,
    ) -> Result<RefAcceptance, io::Error> {
        if let Some(reason) = self.verify(payload_ref, signal).await? {
            self.refuse(payload_ref, reason);
            return Ok(RefAcceptance::Refused(reason));
        }
        {
            let mut state = lock(&self.state);
            state.owned.push(payload_ref.clone());
            state.owned_bytes += payload_ref.bytes;
        }
        (self.send)(RefControl::RefAck {
            ref_id: payload_ref.ref_id.clone(),
        });
        Ok(RefAcceptance::Ok)
    }

    /// Answers `ref_nack` for a ref this side will not take, e.g. one whose job was cancelled before it was opened.
    pub fn refuse(&self, payload_ref: &PayloadRef, reason: RefNackReason) {
        if lock(&self.state)
            .owned
            .iter()
            .any(|owned| owned.ref_id == payload_ref.ref_id)
        {
            return;
        }
        (self.send)(RefControl::RefNack {
            ref_id: payload_ref.ref_id.clone(),
            reason,
        });
    }

    pub async fn consume(&self, ref_id: &str) -> Result<Vec<u8>, PayloadRefFailure> {
        let owned = lock(&self.state)
            .owned
            .iter()
            .find(|owned| owned.ref_id == ref_id)
            .cloned();
        let Some(owned) = owned else {
            return Err(PayloadRefError::new(
                PayloadRefErrorCode::RefNotOwned,
                &format!("ref {ref_id} is not owned by this side"),
                None,
            )
            .into());
        };
        let path = ref_file_path_of(&self.root, &owned)?;
        let bytes = tokio::fs::read(&path).await?;
        match tokio::fs::remove_file(&path).await {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let mut state = lock(&self.state);
        state.owned.retain(|entry| entry.ref_id != ref_id);
        state.owned_bytes -= owned.bytes;
        Ok(bytes)
    }

    pub fn owned_ids(&self) -> Vec<String> {
        lock(&self.state)
            .owned
            .iter()
            .map(|owned| owned.ref_id.clone())
            .collect()
    }

    async fn verify(
        &self,
        payload_ref: &PayloadRef,
        signal: Option<&AbortSignal>,
    ) -> io::Result<Option<RefNackReason>> {
        if is_aborted(signal) {
            return Ok(Some(RefNackReason::Cancelled));
        }
        let (foreign, over_budget) = {
            let state = lock(&self.state);
            (
                payload_ref.generation != self.generation
                    || !is_ref_id_str(&payload_ref.ref_id)
                    || state
                        .owned
                        .iter()
                        .any(|owned| owned.ref_id == payload_ref.ref_id),
                state.owned_bytes + payload_ref.bytes > self.budget,
            )
        };
        if foreign {
            return Ok(Some(RefNackReason::Foreign));
        }
        if over_budget {
            return Ok(Some(RefNackReason::Budget));
        }
        let path = ref_file_path_of(&self.root, payload_ref)?;
        let checked: io::Result<Option<RefNackReason>> = async {
            if tokio::fs::metadata(&path).await?.len() != payload_ref.bytes {
                return Ok(Some(RefNackReason::Corrupt));
            }
            match hash_file(&path, signal).await? {
                None => Ok(Some(RefNackReason::Cancelled)),
                Some(digest) if digest == payload_ref.sha256 => Ok(None),
                Some(_) => Ok(Some(RefNackReason::Corrupt)),
            }
        }
        .await;
        match checked {
            Err(error) if is_missing_file(&error) => Ok(Some(RefNackReason::Missing)),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::task::JoinHandle;

    use super::*;
    use crate::codec::{FrameDecoder, encode_frame};
    use crate::payload_ref_paths::{
        ref_file_path_of, spool_dir, sweep_ref_generations, teardown_ref_generation,
    };
    use crate::payload_wire::{PackedPayload, pack_payload, unpack_payload};
    use crate::protocol::{ChildToServer, ToolChildMessage};

    const MIB: usize = 1024 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(60);

    fn exists(path: &Path) -> bool {
        path.exists()
    }

    fn bytes_of(size: usize, seed: usize) -> Vec<u8> {
        (0..size)
            .map(|index| ((index * 31 + seed) & 0xff) as u8)
            .collect()
    }

    fn writer_at(root: &Path, generation: u64) -> Arc<PayloadRefWriter> {
        Arc::new(PayloadRefWriter::new(PayloadRefWriterOptions::new(
            root, generation,
        )))
    }

    struct Link {
        reader: PayloadRefReader,
        sent: Arc<Mutex<Vec<RefControl>>>,
        deliveries: Arc<Mutex<Vec<JoinHandle<()>>>>,
    }

    impl Link {
        fn sent(&self) -> Vec<RefControl> {
            lock(&self.sent).clone()
        }

        async fn delivered(&self) {
            let handles: Vec<_> = lock(&self.deliveries).drain(..).collect();
            for handle in handles {
                tokio::time::timeout(TIMEOUT, handle)
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }

    /// A fake transport: every ref control the receiver sends is delivered to the writer, in order.
    fn link(root: &Path, writer: &Arc<PayloadRefWriter>, budget: Option<u64>) -> Link {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let deliveries = Arc::new(Mutex::new(Vec::new()));
        let (sent_in, deliveries_in, writer_in) =
            (sent.clone(), deliveries.clone(), writer.clone());
        let reader = PayloadRefReader::new(PayloadRefReaderOptions {
            root: root.to_path_buf(),
            generation: writer.generation,
            budget,
            send: Arc::new(move |control| {
                lock(&sent_in).push(control.clone());
                let writer = writer_in.clone();
                lock(&deliveries_in).push(tokio::spawn(async move {
                    writer.handle(&control).await.unwrap();
                }));
            }),
        });
        Link {
            reader,
            sent,
            deliveries,
        }
    }

    fn received(frame: &[u8]) -> ToolChildMessage {
        FrameDecoder::new()
            .push(frame)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    struct Scenario {
        exists_after_settlement: bool,
        acceptance: RefAcceptance,
        exists_after_ack: bool,
        bytes: Option<Vec<u8>>,
        sha256_verified: bool,
        sent: Vec<RefControl>,
        writer_pending: Vec<String>,
        exists_after_consume: bool,
    }

    impl Scenario {
        fn meets_ownership_transfer(&self) -> bool {
            self.exists_after_settlement
                && self.acceptance.is_ok()
                && self.exists_after_ack
                && self.sha256_verified
                && self.sent.len() == 1
                && matches!(self.sent[0], RefControl::RefAck { .. })
                && self.writer_pending.is_empty()
                && !self.exists_after_consume
        }
    }

    /// The delayed-consumption scenario: the writer sends a 5 MiB `result_ref`, its sender-side operation settles,
    /// and the receiver opens the file only later, behind an event gate.
    async fn delayed_consumption(
        root: &Path,
        on_sender_settled: impl FnOnce(&PayloadRef),
    ) -> Scenario {
        let writer = writer_at(root, 1);
        let link = link(root, &writer, None);
        let payload = bytes_of(5 * MIB, 7);
        let payload_ref = writer.create(&payload).await.unwrap();
        let frame = received(
            &encode_frame(
                &ChildToServer::ResultRef {
                    job_id: "j1".into(),
                    r#ref: payload_ref.clone(),
                }
                .into(),
            )
            .unwrap(),
        );
        let ToolChildMessage::Child(ChildToServer::ResultRef { r#ref: framed, .. }) = frame else {
            panic!("expected result_ref")
        };
        on_sender_settled(&payload_ref);
        let exists_after_settlement = exists(&ref_file_path_of(root, &payload_ref).unwrap());
        // the receiver opens the file only now
        let acceptance = link.reader.accept(&framed, None).await.unwrap();
        link.delivered().await;
        let exists_after_ack = exists(&ref_file_path_of(root, &framed).unwrap());
        let bytes = if acceptance.is_ok() {
            Some(link.reader.consume(&framed.ref_id).await.unwrap())
        } else {
            None
        };
        let sha256_verified = bytes.as_ref().is_some_and(|bytes| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                == payload_ref.sha256
        });
        Scenario {
            exists_after_settlement,
            acceptance,
            exists_after_ack,
            bytes,
            sha256_verified,
            sent: link.sent(),
            writer_pending: writer.pending_ids(),
            exists_after_consume: exists(&ref_file_path_of(root, &payload_ref).unwrap()),
        }
    }

    #[tokio::test]
    async fn delayed_consumption_file_outlives_settlement_ownership_moves_on_ref_ack_owner_deletes()
    {
        let root = tempfile::tempdir().unwrap();
        let result = delayed_consumption(root.path(), |_| {}).await;
        assert!(result.exists_after_settlement);
        assert_eq!(result.acceptance, RefAcceptance::Ok);
        assert_eq!(result.sent.len(), 1);
        assert!(matches!(result.sent[0], RefControl::RefAck { .. }));
        assert!(result.exists_after_ack);
        assert!(result.sha256_verified);
        assert_eq!(result.bytes.as_ref().map(Vec::len), Some(5 * MIB));
        assert!(!result.exists_after_consume);
        assert!(result.meets_ownership_transfer());
    }

    #[tokio::test]
    async fn delayed_consumption_negative_control_old_writer_deletes_on_settlement_rule_is_rejected()
     {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let result = delayed_consumption(&root_path, |payload_ref| {
            std::fs::remove_file(ref_file_path_of(&root_path, payload_ref).unwrap()).unwrap();
        })
        .await;
        assert_eq!(
            result.acceptance,
            RefAcceptance::Refused(RefNackReason::Missing)
        );
        assert!(matches!(
            result.sent.as_slice(),
            [RefControl::RefNack {
                reason: RefNackReason::Missing,
                ..
            }]
        ));
        assert!(!result.meets_ownership_transfer());
    }

    #[tokio::test]
    async fn a_corrupted_file_is_nacked_and_the_writer_deletes_it() {
        let root = tempfile::tempdir().unwrap();
        let writer = writer_at(root.path(), 1);
        let link = link(root.path(), &writer, None);
        let payload_ref = writer.create(&bytes_of(1024, 7)).await.unwrap();
        std::fs::write(
            ref_file_path_of(root.path(), &payload_ref).unwrap(),
            bytes_of(1024, 99),
        )
        .unwrap();
        assert_eq!(
            link.reader.accept(&payload_ref, None).await.unwrap(),
            RefAcceptance::Refused(RefNackReason::Corrupt)
        );
        link.delivered().await;
        assert_eq!(
            link.sent(),
            vec![RefControl::RefNack {
                ref_id: payload_ref.ref_id.clone(),
                reason: RefNackReason::Corrupt
            }]
        );
        assert!(!exists(
            &ref_file_path_of(root.path(), &payload_ref).unwrap()
        ));
        assert!(writer.pending_ids().is_empty());
        assert_eq!(writer.owned_bytes(), 0);
    }

    #[tokio::test]
    async fn cancellation_before_ack_sends_ref_nack_cancelled_and_the_writer_deletes() {
        let root = tempfile::tempdir().unwrap();
        let writer = writer_at(root.path(), 1);
        let link = link(root.path(), &writer, None);
        let first = writer.create(&bytes_of(2048, 7)).await.unwrap();
        let second = writer.create(&bytes_of(2048, 7)).await.unwrap();
        let signal = AbortSignal::new();
        signal.abort();
        assert_eq!(
            link.reader.accept(&first, Some(&signal)).await.unwrap(),
            RefAcceptance::Refused(RefNackReason::Cancelled)
        );
        link.reader.refuse(&second, RefNackReason::Cancelled);
        link.delivered().await;
        assert_eq!(
            link.sent(),
            vec![
                RefControl::RefNack {
                    ref_id: first.ref_id.clone(),
                    reason: RefNackReason::Cancelled
                },
                RefControl::RefNack {
                    ref_id: second.ref_id.clone(),
                    reason: RefNackReason::Cancelled
                },
            ]
        );
        assert!(!exists(&ref_file_path_of(root.path(), &first).unwrap()));
        assert!(!exists(&ref_file_path_of(root.path(), &second).unwrap()));
    }

    #[tokio::test]
    async fn receiver_death_before_ack_generation_teardown_deletes_the_ref_and_spool() {
        let root = tempfile::tempdir().unwrap();
        let writer = writer_at(root.path(), 4);
        let payload_ref = writer.create(&bytes_of(4096, 7)).await.unwrap();
        received(
            &encode_frame(
                &ChildToServer::ResultRef {
                    job_id: "j1".into(),
                    r#ref: payload_ref.clone(),
                }
                .into(),
            )
            .unwrap(),
        );
        std::fs::create_dir_all(spool_dir(root.path(), 4)).unwrap();
        std::fs::write(spool_dir(root.path(), 4).join("entry-1.chunk"), "spooled").unwrap();
        writer.teardown().await.unwrap();
        assert!(!exists(
            &ref_file_path_of(root.path(), &payload_ref).unwrap()
        ));
        assert!(!exists(&spool_dir(root.path(), 4)));
        assert!(writer.pending_ids().is_empty());
    }

    #[tokio::test]
    async fn the_generation_sweep_removes_every_generation_that_is_not_live() {
        let root = tempfile::tempdir().unwrap();
        let stale = writer_at(root.path(), 2);
        let stale_ref = stale.create(&bytes_of(64, 7)).await.unwrap();
        std::fs::create_dir_all(spool_dir(root.path(), 2)).unwrap();
        let live = writer_at(root.path(), 3);
        let live_ref = live.create(&bytes_of(64, 7)).await.unwrap();
        let swept = sweep_ref_generations(root.path(), &std::collections::HashSet::from([3]))
            .await
            .unwrap();
        assert_eq!(swept, vec![2]);
        assert!(!exists(&ref_file_path_of(root.path(), &stale_ref).unwrap()));
        assert!(!exists(&spool_dir(root.path(), 2)));
        assert!(exists(&ref_file_path_of(root.path(), &live_ref).unwrap()));
    }

    #[tokio::test]
    async fn an_acked_ref_whose_owner_dies_is_deleted_by_the_teardown_sweep() {
        let root = tempfile::tempdir().unwrap();
        let writer = writer_at(root.path(), 1);
        let link = link(root.path(), &writer, None);
        let payload_ref = writer.create(&bytes_of(8192, 7)).await.unwrap();
        assert_eq!(
            link.reader.accept(&payload_ref, None).await.unwrap(),
            RefAcceptance::Ok
        );
        link.delivered().await;
        assert!(writer.pending_ids().is_empty());
        assert_eq!(link.reader.owned_ids(), vec![payload_ref.ref_id.clone()]);
        assert!(exists(
            &ref_file_path_of(root.path(), &payload_ref).unwrap()
        ));
        teardown_ref_generation(root.path(), 1).await.unwrap();
        assert!(!exists(
            &ref_file_path_of(root.path(), &payload_ref).unwrap()
        ));
    }

    #[tokio::test]
    async fn refuses_a_ref_past_refs_budget_with_ref_budget_exceeded_on_both_sides() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            writer_at(root.path(), 1).budget,
            TOOL_CHILD_LIMITS.refs_budget
        );
        let writer = Arc::new(PayloadRefWriter::new(PayloadRefWriterOptions {
            budget: Some(6 * MIB as u64),
            ..PayloadRefWriterOptions::new(root.path(), 1)
        }));
        let first = writer.create(&bytes_of(5 * MIB, 7)).await.unwrap();
        for _ in 0..2 {
            let error = writer.create(&bytes_of(2 * MIB, 7)).await.unwrap_err();
            assert_eq!(error.code(), Some(PayloadRefErrorCode::RefBudgetExceeded));
        }
        let names: Vec<_> = std::fs::read_dir(
            ref_file_path_of(root.path(), &first)
                .unwrap()
                .parent()
                .unwrap(),
        )
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
        assert_eq!(names, vec![first.ref_id.clone()]);
        let link = link(root.path(), &writer, Some(4 * MIB as u64));
        assert_eq!(
            link.reader.accept(&first, None).await.unwrap(),
            RefAcceptance::Refused(RefNackReason::Budget)
        );
        link.delivered().await;
        assert_eq!(
            link.sent(),
            vec![RefControl::RefNack {
                ref_id: first.ref_id.clone(),
                reason: RefNackReason::Budget
            }]
        );
        assert_eq!(writer.owned_bytes(), 0);
        writer.create(&bytes_of(5 * MIB, 7)).await.unwrap();
    }

    #[tokio::test]
    async fn reports_an_unacknowledged_ref_overdue_after_ref_ack_ms_without_deleting_it() {
        let root = tempfile::tempdir().unwrap();
        let now = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let clock = now.clone();
        let writer = PayloadRefWriter::new(PayloadRefWriterOptions {
            now: Some(Arc::new(move || clock.load(Ordering::SeqCst))),
            ..PayloadRefWriterOptions::new(root.path(), 1)
        });
        let payload_ref = writer.create(&bytes_of(16, 7)).await.unwrap();
        now.fetch_add(TOOL_CHILD_LIMITS.ref_ack_ms - 1, Ordering::SeqCst);
        assert!(writer.overdue().is_empty());
        now.fetch_add(1, Ordering::SeqCst);
        assert_eq!(writer.overdue(), vec![payload_ref.ref_id.clone()]);
        assert!(exists(
            &ref_file_path_of(root.path(), &payload_ref).unwrap()
        ));
    }

    #[tokio::test]
    async fn a_17_mib_host_call_args_child_to_server_round_trips_by_acknowledged_reference() {
        let root = tempfile::tempdir().unwrap();
        let child_writer = writer_at(root.path(), 5);
        let link = link(root.path(), &child_writer, None);
        let args = json!({"path": "big.txt", "content": "a".repeat(17 * MIB)});
        let host_call = |args: Option<Value>, args_ref: Option<PayloadRef>| -> ToolChildMessage {
            ChildToServer::HostCall {
                call_id: "c1".into(),
                job_id: "j1".into(),
                kind: crate::protocol::HostCallKind::Tool,
                name: Some("write".into()),
                args,
                args_ref,
            }
            .into()
        };
        assert!(encode_frame(&host_call(Some(args.clone()), None)).is_err());
        let PackedPayload::Ref(packed) = pack_payload(&args, &child_writer).await.unwrap() else {
            panic!("17 MiB args must move by reference")
        };
        let frame = received(&encode_frame(&host_call(None, Some(packed.clone()))).unwrap());
        let ToolChildMessage::Child(ChildToServer::HostCall {
            args_ref: Some(args_ref),
            ..
        }) = frame
        else {
            panic!("expected host_call with argsRef")
        };
        assert_eq!(
            unpack_payload(&args_ref, &link.reader, None).await.unwrap(),
            args
        );
        link.delivered().await;
        assert_eq!(
            link.sent(),
            vec![RefControl::RefAck {
                ref_id: packed.ref_id.clone()
            }]
        );
        assert!(!exists(&ref_file_path_of(root.path(), &packed).unwrap()));
        assert!(child_writer.pending_ids().is_empty());
    }

    #[tokio::test]
    async fn a_20_mib_host_reply_value_server_to_child_round_trips_by_acknowledged_reference() {
        let root = tempfile::tempdir().unwrap();
        let server_writer = writer_at(root.path(), 5);
        let link = link(root.path(), &server_writer, None);
        let value = Value::String("v".repeat(20 * MIB));
        let PackedPayload::Ref(packed) = pack_payload(&value, &server_writer).await.unwrap() else {
            panic!("20 MiB value must move by reference")
        };
        let frame = received(
            &encode_frame(
                &crate::protocol::ServerToChild::HostReply {
                    call_id: "c1".into(),
                    value: None,
                    value_ref: Some(packed.clone()),
                    error: None,
                }
                .into(),
            )
            .unwrap(),
        );
        let ToolChildMessage::Server(crate::protocol::ServerToChild::HostReply {
            value_ref: Some(value_ref),
            ..
        }) = frame
        else {
            panic!("expected host_reply with valueRef")
        };
        assert_eq!(
            unpack_payload(&value_ref, &link.reader, None)
                .await
                .unwrap(),
            value
        );
        link.delivered().await;
        assert_eq!(
            link.sent(),
            vec![RefControl::RefAck {
                ref_id: packed.ref_id.clone()
            }]
        );
        assert!(!exists(&ref_file_path_of(root.path(), &packed).unwrap()));
    }

    #[tokio::test]
    async fn keeps_a_payload_at_or_below_ref_threshold_inline_and_writes_no_file() {
        let root = tempfile::tempdir().unwrap();
        let writer = writer_at(root.path(), 1);
        let packed = pack_payload(&json!({"small": true}), &writer)
            .await
            .unwrap();
        assert!(
            matches!(packed, PackedPayload::Inline(ref value) if *value == json!({"small": true}))
        );
        assert!(!exists(&root.path().join("gen-1")));
    }

    #[tokio::test]
    async fn unpack_payload_rejects_a_ref_it_had_to_nack() {
        let root = tempfile::tempdir().unwrap();
        let writer = writer_at(root.path(), 1);
        let link = link(root.path(), &writer, None);
        let big = Value::String("x".repeat(TOOL_CHILD_LIMITS.ref_threshold + 1));
        let PackedPayload::Ref(packed) = pack_payload(&big, &writer).await.unwrap() else {
            panic!("expected a ref")
        };
        std::fs::remove_file(ref_file_path_of(root.path(), &packed).unwrap()).unwrap();
        let error = unpack_payload(&packed, &link.reader, None)
            .await
            .unwrap_err();
        let PayloadRefFailure::Ref(error) = error else {
            panic!("expected a PayloadRefError")
        };
        assert_eq!(error.code, PayloadRefErrorCode::RefRejected);
        assert_eq!(error.reason, Some(RefNackReason::Missing));
        link.delivered().await;
    }
}
