pub type WalErrorCallback = Arc<dyn Fn(&io::Error, usize) + Send + Sync>;
pub type WalWriteCallback = Arc<dyn Fn(&mut File, &[u8]) -> io::Result<usize> + Send + Sync>;
pub type TruncateCallback = Option<Arc<dyn Fn(&File, u64) -> io::Result<()> + Send + Sync>>;
use crate::otlp_spool_dir::{active_name, batch_name, delete_file, publish_batch};
use crate::otlp_spool_file::{Signal, field, frame, varint};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};
pub struct SpoolWriterInput {
    pub dir: PathBuf,
    pub signal: Signal,
    pub pid: u32,
    pub boot_id: String,
    pub header: Vec<u8>,
    pub consent: Arc<dyn Fn() -> bool + Send + Sync>,
    pub on_error: WalErrorCallback,
    pub write: Option<WalWriteCallback>,
    pub truncate: TruncateCallback,
}
pub struct SpoolWriter {
    input: SpoolWriterInput,
    pub name: String,
    file: Option<File>,
    pub records: usize,
    pub bytes: u64,
}
impl SpoolWriter {
    pub fn new(input: SpoolWriterInput) -> Self {
        let name = active_name(input.signal, input.pid, &input.boot_id);
        Self {
            input,
            name,
            file: None,
            records: 0,
            bytes: 0,
        }
    }
    pub fn signal(&self) -> Signal {
        self.input.signal
    }
    pub fn append(&mut self, bytes: &[u8]) -> bool {
        self.append_encoded(|| Ok(bytes.to_vec()))
    }
    pub fn append_encoded(&mut self, encode: impl FnOnce() -> io::Result<Vec<u8>>) -> bool {
        if !(self.input.consent)() {
            return false;
        }
        let result = (|| {
            let bytes = frame(&encode()?);
            if self.file.is_none() {
                self.open()?;
            }
            let mut file = self
                .file
                .take()
                .ok_or_else(|| io::Error::other("spool is not open"))?;
            match self.write_all(&mut file, &bytes) {
                Ok(()) => {
                    self.file = Some(file);
                    self.records += 1;
                    self.bytes += bytes.len() as u64;
                    Ok(())
                }
                Err(error) => {
                    let rollback = match &self.input.truncate {
                        Some(f) => f(&file, self.bytes),
                        None => OpenOptions::new()
                            .write(true)
                            .open(self.input.dir.join(&self.name))
                            .and_then(|f| f.set_len(self.bytes)),
                    };
                    if let Err(e) = rollback {
                        (self.input.on_error)(&e, self.records);
                        self.file = Some(file);
                        self.discard()?;
                    } else {
                        self.file = Some(file);
                    }
                    Err(error)
                }
            }
        })();
        match result {
            Ok(()) => true,
            Err(e) => {
                (self.input.on_error)(&e, 1);
                false
            }
        }
    }
    fn write_all(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        let mut at = 0;
        while at < bytes.len() {
            let n = match &self.input.write {
                Some(f) => f(file, &bytes[at..])?,
                None => file.write(&bytes[at..])?,
            };
            if n == 0 || n > bytes.len() - at {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "spool write made no progress",
                ));
            }
            at += n;
        }
        Ok(())
    }
    fn open(&mut self) -> io::Result<()> {
        let path = self.input.dir.join(&self.name);
        let open = || {
            let mut options = OpenOptions::new();
            options.append(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(&path)
        };
        let mut file = match open() {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&self.input.dir)?;
                open()?
            }
            Err(e) => return Err(e),
        };
        let header = frame(&self.input.header);
        if let Err(e) = self.write_all(&mut file, &header) {
            drop(file);
            if let Err(cleanup) = delete_file(&self.input.dir, &self.name) {
                return Err(io::Error::new(
                    e.kind(),
                    format!("{e}; header cleanup failed: {cleanup}"),
                ));
            }
            return Err(e);
        }
        self.file = Some(file);
        self.bytes = header.len() as u64;
        Ok(())
    }
    pub fn seal(&mut self, batch_id: &str) -> io::Result<Option<String>> {
        if self.file.is_none() || self.records == 0 {
            return Ok(None);
        }
        self.close();
        let sealed = batch_name(self.input.signal, batch_id);
        Ok(publish_batch(&self.input.dir, &self.name, &sealed)?.then_some(sealed))
    }
    pub fn discard(&mut self) -> io::Result<()> {
        let open = self.file.is_some();
        self.close();
        if open {
            delete_file(&self.input.dir, &self.name)?;
        }
        Ok(())
    }
    pub fn close(&mut self) {
        self.file = None;
        self.records = 0;
        self.bytes = 0;
    }
}
fn event_name(name: &str) -> bool {
    let Some(bare) = name.strip_prefix("omo.") else {
        return false;
    };
    !bare.is_empty()
        && bare.len() <= 63
        && bare.as_bytes()[0].is_ascii_lowercase()
        && bare
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn wire_key(key: &str) -> bool {
    !key.starts_with('$')
        && !matches!(
            key,
            "ci" | "event_id"
                | "install_hash"
                | "package_version"
                | "platform"
                | "product_name"
                | "schema_version"
                | "surface"
        )
}
fn any_value(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match value {
        Value::String(s) => field(&mut out, 1, s.as_bytes()),
        Value::Bool(b) => {
            varint(&mut out, 2 * 8);
            varint(&mut out, u64::from(*b));
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                varint(&mut out, 3 * 8);
                varint(&mut out, i as u64);
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0
                    && (crate::otlp_encode::INT64_MIN..crate::otlp_encode::INT64_LIMIT).contains(&f)
                {
                    varint(&mut out, 3 * 8);
                    varint(&mut out, (f as i64) as u64);
                } else {
                    varint(&mut out, 4 * 8 + 1);
                    out.extend_from_slice(&f.to_le_bytes());
                }
            }
        }
        Value::Array(values) => {
            let mut array = Vec::new();
            for v in values {
                field(&mut array, 1, &any_value(v));
            }
            field(&mut out, 5, &array);
        }
        Value::Object(values) => {
            let mut kv = Vec::new();
            attributes(&mut kv, 1, values, |_| true);
            field(&mut out, 6, &kv);
        }
        Value::Null => {}
    }
    out
}
fn attributes(
    out: &mut Vec<u8>,
    number: u64,
    attrs: &Map<String, Value>,
    allowed: impl Fn(&str) -> bool,
) {
    for (key, value) in attrs {
        if allowed(key) {
            let mut pair = Vec::new();
            field(&mut pair, 1, key.as_bytes());
            field(&mut pair, 2, &any_value(value));
            field(out, number, &pair);
        }
    }
}
fn fixed64(out: &mut Vec<u8>, n: u64, value: u64) {
    if value != 0 {
        varint(out, n * 8 + 1);
        out.extend_from_slice(&value.to_le_bytes());
    }
}
fn integer(out: &mut Vec<u8>, n: u64, value: u64) {
    if value != 0 {
        varint(out, n * 8);
        varint(out, value);
    }
}
fn flags(out: &mut Vec<u8>, n: u64, value: u8) {
    if value != 0 {
        varint(out, n * 8 + 5);
        out.extend_from_slice(&u32::from(value).to_le_bytes());
    }
}
#[derive(Default, Clone)]
pub struct WalLogRecord {
    pub event_name: Option<String>,
    pub time_unix_nano: u64,
    pub observed_time_unix_nano: u64,
    pub severity_number: u32,
    pub attributes: Map<String, Value>,
    pub dropped_attributes_count: u32,
    pub trace_id: Vec<u8>,
    pub span_id: Vec<u8>,
    pub trace_flags: u8,
}
pub struct WalLogRecordProcessor {
    pub writer: SpoolWriter,
    on_appended: Box<dyn FnMut(&mut SpoolWriter) + Send>,
}
impl WalLogRecordProcessor {
    pub fn new(writer: SpoolWriter, on_appended: Box<dyn FnMut(&mut SpoolWriter) + Send>) -> Self {
        Self {
            writer,
            on_appended,
        }
    }
    pub fn on_emit(&mut self, record: &WalLogRecord) {
        let Some(name) = record.event_name.as_deref().filter(|s| event_name(s)) else {
            return;
        };
        let mut bytes = Vec::new();
        fixed64(&mut bytes, 1, record.time_unix_nano);
        fixed64(&mut bytes, 11, record.observed_time_unix_nano);
        integer(
            &mut bytes,
            2,
            if record.severity_number <= 24 {
                u64::from(record.severity_number)
            } else {
                0
            },
        );
        attributes(&mut bytes, 6, &record.attributes, wire_key);
        integer(&mut bytes, 7, u64::from(record.dropped_attributes_count));
        if !record.trace_id.is_empty() {
            field(&mut bytes, 9, &record.trace_id);
        }
        if !record.span_id.is_empty() {
            field(&mut bytes, 10, &record.span_id);
        }
        flags(&mut bytes, 8, record.trace_flags);
        field(&mut bytes, 12, name.as_bytes());
        if self.writer.append(&bytes) {
            (self.on_appended)(&mut self.writer);
        }
    }
    pub fn shutdown(&mut self) {
        self.writer.close();
    }
}
pub type SpanAllowlist = HashMap<String, Vec<String>>;
#[derive(Default, Clone)]
pub struct WalSpan {
    pub name: String,
    pub trace_id: Vec<u8>,
    pub span_id: Vec<u8>,
    pub parent_span_id: Vec<u8>,
    pub trace_flags: u8,
    pub kind: u32,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub attributes: Map<String, Value>,
    pub dropped_attributes_count: u32,
    pub status_code: u32,
}
pub struct WalSpanProcessor {
    pub writer: SpoolWriter,
    on_appended: Box<dyn FnMut(&mut SpoolWriter) + Send>,
    spans: SpanAllowlist,
}
impl WalSpanProcessor {
    pub fn new(
        writer: SpoolWriter,
        on_appended: Box<dyn FnMut(&mut SpoolWriter) + Send>,
        spans: SpanAllowlist,
    ) -> Self {
        Self {
            writer,
            on_appended,
            spans,
        }
    }
    pub fn on_end(&mut self, span: &WalSpan) {
        let Some(allowed) = self.spans.get(&span.name) else {
            return;
        };
        let mut bytes = Vec::new();
        field(&mut bytes, 1, &span.trace_id);
        field(&mut bytes, 2, &span.span_id);
        if !span.parent_span_id.is_empty() {
            field(&mut bytes, 4, &span.parent_span_id);
        }
        field(&mut bytes, 5, span.name.as_bytes());
        integer(&mut bytes, 6, u64::from(span.kind + 1));
        fixed64(&mut bytes, 7, span.start_time_unix_nano);
        fixed64(&mut bytes, 8, span.end_time_unix_nano);
        attributes(&mut bytes, 9, &span.attributes, |k| {
            wire_key(k) && allowed.iter().any(|a| a == k)
        });
        integer(&mut bytes, 10, u64::from(span.dropped_attributes_count));
        let mut status = Vec::new();
        integer(&mut status, 3, u64::from(span.status_code));
        field(&mut bytes, 15, &status);
        flags(&mut bytes, 16, span.trace_flags);
        if self.writer.append(&bytes) {
            (self.on_appended)(&mut self.writer);
        }
    }
    pub fn shutdown(&mut self) {
        self.writer.close();
    }
}
