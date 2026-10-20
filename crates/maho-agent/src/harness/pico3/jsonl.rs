use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, fs::{self, File, OpenOptions}, io::Write as IoWrite, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use maho_chord::delta::is_base;
use super::{memory::{storage_error, MemoryStorage, MAX_SAFE_ID}, types::*};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
struct Record {
    seq: Seq,
    max_id: Id,
    writes: Vec<Write>,
    #[serde(default, skip_serializing_if="Vec::is_empty")]
    refs: Vec<String>,
}
struct Files { main: Option<File>, sticky: BTreeMap<Id, File>, tasks: BTreeMap<Id, File> }
pub struct JsonlStorage {
    memory: MemoryStorage,
    files: RefCell<Files>,
    dir: PathBuf,
    pub fsync: bool,
}

fn io(error: std::io::Error) -> Error { storage_error(error.to_string()) }
fn append_file(path: &Path) -> Result<File> { OpenOptions::new().create(true).append(true).open(path).map_err(io) }
fn truncate_to(path: &Path, end: u64) -> Result<()> {
    OpenOptions::new().write(true).open(path).map_err(io)?.set_len(end).map_err(io)
}
fn truncate_torn_tail(path: &Path) -> Result<()> {
    let bytes = fs::read(path).map_err(io)?;
    let end = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |index| index.saturating_add(1));
    if end != bytes.len() { truncate_to(path, u64::try_from(end).map_err(|_| storage_error("file length overflow"))?)?; }
    Ok(())
}
fn read_records(path: &Path) -> Result<Vec<(Record, u64)>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io(error)),
    };
    let mut records = Vec::new();
    let mut last = 0;
    let mut end = 0_u64;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        end = end.checked_add(u64::try_from(line.len()).map_err(|_| storage_error("file length overflow"))?).ok_or_else(|| storage_error("file length overflow"))?;
        if line.last() != Some(&b'\n') { break; }
        let line = &line[..line.len().saturating_sub(1)];
        if line.is_empty() { continue; }
        let record: Record = serde_json::from_slice(line).map_err(|error| storage_error(format!("{}: malformed record or lacks seq/maxId/writes: {error}", path.display())))?;
        if record.seq <= last || record.seq > MAX_SAFE_ID || record.max_id > MAX_SAFE_ID {
            return Err(storage_error(format!("{}: sequence not increasing or unsafe at {}", path.display(), record.seq)));
        }
        for reference in &record.refs {
            if sidecar_id(reference, "task-").is_none() && sidecar_id(reference, "sticky-").is_none() {
                return Err(storage_error("invalid sidecar reference"));
            }
        }
        last = record.seq;
        records.push((record, end));
    }
    Ok(records)
}
fn sidecar_id(file: &str, prefix: &str) -> Option<Id> {
    file.strip_prefix(prefix)?.strip_suffix(".jsonl")?.parse::<Id>().ok().filter(|id| *id > 0 && *id <= MAX_SAFE_ID)
}
fn record_base(record: &Record) -> bool {
    let operations: Vec<_> = record.writes.iter().flat_map(|write| match write {
        Write::Doc { ops, .. } => ops.clone(),
        Write::Conversation { .. } | Write::Entry { .. } | Write::Task { .. } | Write::TaskPatch { .. } | Write::Input { .. } => Vec::new(),
    }).collect();
    is_base(&operations)
}

impl JsonlStorage {
    pub fn open(dir: impl AsRef<Path>, fsync: bool) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(io)?;
        for entry in fs::read_dir(&dir).map_err(io)? {
            let path = entry.map_err(io)?.path();
            if path.extension().is_some_and(|extension| extension == "jsonl") { truncate_torn_tail(&path)?; }
        }
        let main = append_file(&dir.join("main.jsonl"))?;
        let storage = Self { memory: MemoryStorage::new(), files: RefCell::new(Files { main: Some(main), sticky: BTreeMap::new(), tasks: BTreeMap::new() }), dir, fsync };
        storage.replay()?;
        Ok(storage)
    }
    pub fn sizes(&self) -> Result<BTreeMap<String, u64>> {
        let mut sizes = BTreeMap::new();
        for entry in fs::read_dir(&self.dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().into_string().map_err(|_| storage_error("non UTF-8 storage filename"))?;
            sizes.insert(name, entry.metadata().map_err(io)?.len());
        }
        Ok(sizes)
    }
    fn replay(&self) -> Result<()> {
        let main = read_records(&self.dir.join("main.jsonl"))?;
        let confirmed: BTreeMap<_, BTreeSet<_>> = main.iter().map(|(record, _)| (record.seq, record.refs.iter().cloned().collect())).collect();
        let mut batches: BTreeMap<Seq, Vec<Write>> = BTreeMap::new();
        let mut retired = BTreeMap::new();
        let mut found: BTreeMap<Seq, BTreeSet<String>> = BTreeMap::new();
        let mut bases = BTreeMap::new();
        let mut max_id = 0;
        for (record, _) in &main {
            for write in &record.writes {
                if let Write::TaskPatch { patch } = write
                    && patch.status == Some(TaskStatus::Terminal) { retired.insert(patch.id, record.seq); }
            }
            batches.insert(record.seq, record.writes.clone());
            max_id = max_id.max(record.max_id);
        }
        let mut sidecars = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            let file = entry.file_name().into_string().map_err(|_| storage_error("non UTF-8 storage filename"))?;
            if sidecar_id(&file, "task-").is_some() || sidecar_id(&file, "sticky-").is_some() { sidecars.push(file); }
        }
        for file in &sidecars {
            let path = self.dir.join(file);
            let records = read_records(&path)?;
            if file.starts_with("sticky-")
                && let Some((record, _)) = records.first()
                && record_base(record) { bases.insert(file.clone(), record.seq); }
            let mut confirmed_end = 0;
            let mut unconfirmed = false;
            for (record, end) in records {
                if !confirmed.get(&record.seq).is_some_and(|refs| refs.contains(file)) { unconfirmed = true; continue; }
                if unconfirmed { return Err(storage_error(format!("{}: confirmed record follows an unconfirmed tail", path.display()))); }
                confirmed_end = end;
                found.entry(record.seq).or_default().insert(file.clone());
                let batch = batches.get_mut(&record.seq).ok_or_else(|| storage_error("missing main publication"))?;
                batch.extend(record.writes);
                max_id = max_id.max(record.max_id);
            }
            if unconfirmed { truncate_to(&path, confirmed_end)?; }
        }
        for (record, _) in &main {
            for file in &record.refs {
                if found.get(&record.seq).is_some_and(|files| files.contains(file)) { continue; }
                if bases.get(file).is_some_and(|seq| record.seq < *seq) { continue; }
                if sidecar_id(file, "task-").and_then(|id| retired.get(&id)).is_some_and(|seq| record.seq < *seq) { continue; }
                return Err(storage_error(format!("{file}: missing record for published sequence {}", record.seq)));
            }
        }
        for (seq, writes) in batches {
            let staged = self.memory.stage(&writes, seq)?;
            self.memory.publish(staged);
        }
        {
            let mut state = self.memory.state.borrow_mut();
            state.next_id = state.next_id.max(max_id.saturating_add(1));
        }
        for file in sidecars {
            if let Some(id) = sidecar_id(&file, "task-")
                && self.memory.state.borrow().tasks.get(&id).is_none_or(|task| task.status == TaskStatus::Terminal) {
                fs::remove_file(self.dir.join(file)).map_err(io)?;
            }
        }
        Ok(())
    }
    fn append(&self, file: &mut File, record: &Record) -> Result<()> {
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        file.write_all(&bytes).map_err(io)?;
        if self.fsync { file.sync_all().map_err(io)?; }
        Ok(())
    }
    fn commit_batch(&self, writes: &[Write]) -> Result<Seq> {
        let seq = self.memory.state.borrow().seq.checked_add(1).filter(|seq| *seq <= MAX_SAFE_ID).ok_or_else(|| storage_error("sequence exhausted"))?;
        let staged = self.memory.stage(writes, seq)?;
        let max_id = staged.next_id.saturating_sub(1);
        let mut main = Vec::new();
        let mut sticky: BTreeMap<Id, Vec<Write>> = BTreeMap::new();
        let mut tasks: BTreeMap<Id, Vec<Write>> = BTreeMap::new();
        let mut retired = Vec::new();
        for write in writes {
            match write {
                Write::Doc { r#ref: DocRef::Sticky { conversation_id }, .. } => sticky.entry(*conversation_id).or_default().push(write.clone()),
                Write::TaskPatch { patch } if patch.status != Some(TaskStatus::Terminal) => tasks.entry(patch.id).or_default().push(write.clone()),
                _ => {
                    main.push(write.clone());
                    if let Write::TaskPatch { patch } = write
                        && patch.status == Some(TaskStatus::Terminal) { retired.push(patch.id); }
                }
            }
        }
        let mut refs = Vec::new();
        let mut files = self.files.borrow_mut();
        if files.main.is_none() { return Err(storage_error("storage closed")); }
        for (id, writes) in tasks {
            let name = format!("task-{id}.jsonl");
            if let std::collections::btree_map::Entry::Vacant(entry) = files.tasks.entry(id) { entry.insert(append_file(&self.dir.join(&name))?); }
            let file = files.tasks.get_mut(&id).ok_or_else(|| storage_error("task sidecar not opened"))?;
            self.append(file, &Record { seq, max_id, writes, refs: Vec::new() })?;
            refs.push(name);
        }
        for (id, writes) in sticky {
            let name = format!("sticky-{id}.jsonl");
            if let std::collections::btree_map::Entry::Vacant(entry) = files.sticky.entry(id) { entry.insert(append_file(&self.dir.join(&name))?); }
            let file = files.sticky.get_mut(&id).ok_or_else(|| storage_error("sticky sidecar not opened"))?;
            self.append(file, &Record { seq, max_id, writes, refs: Vec::new() })?;
            refs.push(name);
        }
        // Main is the publication point: all referenced sidecars are flushed first.
        self.append(files.main.as_mut().ok_or_else(|| storage_error("storage closed"))?, &Record { seq, max_id, writes: main, refs })?;
        self.memory.publish(staged);
        for id in retired {
            files.tasks.remove(&id);
            match fs::remove_file(self.dir.join(format!("task-{id}.jsonl"))) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(io(error)),
            }
        }
        Ok(seq)
    }
    fn truncate_sticky(&self, conversation_id: Id) -> Result<()> {
        let path = self.dir.join(format!("sticky-{conversation_id}.jsonl"));
        let records = read_records(&path)?;
        let start = records.iter().rposition(|(record, _)| record_base(record)).unwrap_or(0);
        if start == 0 { return Ok(()); }
        let tmp = path.with_extension("jsonl.tmp");
        let result = (|| {
            let mut file = File::create(&tmp).map_err(io)?;
            for (record, _) in records.into_iter().skip(start) { self.append(&mut file, &record)?; }
            drop(file);
            self.files.borrow_mut().sticky.remove(&conversation_id);
            fs::rename(&tmp, &path).map_err(io)?;
            self.files.borrow_mut().sticky.insert(conversation_id, append_file(&path)?);
            Ok(())
        })();
        if result.is_err() {
            match fs::remove_file(&tmp) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => tracing::warn!(%error, "sticky temporary file cleanup failed"),
            }
        }
        result
    }
}

impl Storage for JsonlStorage {
    fn mint_id(&self) -> Result<Id> { self.memory.mint_id() }
    fn commit<'a>(&'a self, writes: &'a [Write], _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Seq>> { Box::pin(async move { self.commit_batch(writes) }) }
    fn conversation<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Conversation>>> { self.memory.conversation(id, ctx) }
    fn conversations<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Vec<Conversation>>> { self.memory.conversations(ctx) }
    fn entries<'a>(&'a self, ids: &'a [Id], ctx: &'a Context) -> LocalBoxFuture<'a, Result<BTreeMap<Id, Entry>>> { self.memory.entries(ids, ctx) }
    fn scan_entries<'a>(&'a self, scan: EntryScan, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Vec<Entry>>> { self.memory.scan_entries(scan, ctx) }
    fn task<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Task>>> { self.memory.task(id, ctx) }
    fn scan_tasks<'a>(&'a self, scan: TaskScan, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Vec<Task>>> { self.memory.scan_tasks(scan, ctx) }
    fn input<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> { self.memory.input(id, ctx) }
    fn input_by_request<'a>(&'a self, id: Id, request: &'a str, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> { self.memory.input_by_request(id, request, ctx) }
    fn doc<'a>(&'a self, reference: DocRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> { self.memory.doc(reference, ctx) }
    fn doc_as_of<'a>(&'a self, id: Id, at: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> { self.memory.doc_as_of(id, at, ctx) }
    fn truncate<'a>(&'a self, reference: DocRef, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> { Box::pin(async move {
        self.memory.truncate(reference.clone(), ctx).await?;
        if let DocRef::Sticky { conversation_id } = reference { self.truncate_sticky(conversation_id)?; }
        Ok(())
    }) }
    fn close<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> { Box::pin(async move {
        self.memory.close(ctx).await?;
        let mut files = self.files.borrow_mut();
        files.main.take();
        files.tasks.clear();
        files.sticky.clear();
        Ok(())
    }) }
}

#[cfg(test)]
#[path = "jsonl_tests.rs"]
mod tests;
