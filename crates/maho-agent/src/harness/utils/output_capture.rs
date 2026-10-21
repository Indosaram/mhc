//! Port of senpi packages/agent/src/harness/utils/output-capture.ts.

use std::sync::{Arc, Mutex};

use super::adaptive_publisher::{
    AdaptivePublisher, AdaptivePublisherOptions, PublisherClock, SystemPublisherClock,
};
use super::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, truncate_head,
    truncate_tail, utf8_byte_length,
};
use crate::harness::context::Context;
use crate::harness::types::{
    ShellOutputCaptureOptions, ShellOutputLimits, ShellOutputMetadata, ShellOutputRetention,
    ShellOutputTruncation, ShellOutputUpdate, ShellOutputView, TruncationLimit,
};

/// `OUTPUT_MIN_EMIT_INTERVAL_MS`.
pub const OUTPUT_MIN_EMIT_INTERVAL_MS: u64 = 100;

/// `OUTPUT_TARGET_BYTES_PER_SECOND`.
pub const OUTPUT_TARGET_BYTES_PER_SECOND: u64 = 100 * 1024;

/// `OutputCaptureHandlers.onUpdate`.
pub type OutputUpdateHandler = crate::harness::types::ShellOutputUpdateHandler;

/// `OutputCaptureHandlers`.
pub struct OutputCaptureHandlers {
    pub on_update: Option<OutputUpdateHandler>,
    pub on_error: Arc<dyn Fn(String) + Send + Sync>,
}

struct CaptureState {
    buffer: String,
    buffer_bytes: u64,
    total_bytes: u64,
    newlines: u64,
    ends_with_newline: bool,
    current_line_bytes: u64,
    spill_path: Option<String>,
    disposed: bool,
    pending: Vec<u8>,
    decoder_at_start: bool,
}

type CallbackTasks = Arc<Mutex<Vec<tokio::task::JoinHandle<Result<(), String>>>>>;

/// `OutputCapture`: maintains and publishes one bounded shell-output view.
pub struct OutputCapture {
    max_bytes: u64,
    max_lines: u64,
    retain: ShellOutputRetention,
    context: Context,
    on_update: Option<OutputUpdateHandler>,
    state: Arc<Mutex<CaptureState>>,
    publisher: AdaptivePublisher<ShellOutputView, ShellOutputUpdate>,
    callbacks: CallbackTasks,
}

impl OutputCapture {
    pub fn new(
        options: Option<&ShellOutputCaptureOptions>,
        context: Context,
        handlers: OutputCaptureHandlers,
    ) -> Result<Self, String> {
        Self::with_clock(
            options,
            context,
            handlers,
            Arc::new(SystemPublisherClock::default()),
        )
    }

    pub fn with_clock(
        options: Option<&ShellOutputCaptureOptions>,
        context: Context,
        handlers: OutputCaptureHandlers,
        clock: Arc<dyn PublisherClock>,
    ) -> Result<Self, String> {
        let limits = options
            .map(|options| options.limits)
            .unwrap_or(ShellOutputLimits {
                max_bytes: DEFAULT_MAX_BYTES,
                max_lines: DEFAULT_MAX_LINES,
                retain: None,
            });
        let max_bytes = limits.max_bytes;
        let max_lines = limits.max_lines;
        let retain = limits.retain.unwrap_or(ShellOutputRetention::Tail);
        if max_bytes == 0 {
            return Err("Output maxBytes must be a positive finite number".to_string());
        }
        if max_lines == 0 {
            return Err("Output maxLines must be a positive integer".to_string());
        }

        let state = Arc::new(Mutex::new(CaptureState {
            buffer: String::new(),
            buffer_bytes: 0,
            total_bytes: 0,
            newlines: 0,
            ends_with_newline: true,
            current_line_bytes: 0,
            spill_path: None,
            disposed: false,
            pending: Vec::new(),
            decoder_at_start: true,
        }));
        let on_update = handlers.on_update.clone();
        let publisher_context = context.clone();
        let snapshot_state = state.clone();
        let on_error = handlers.on_error.clone();
        let callback_error = handlers.on_error.clone();
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let pending_callbacks = callbacks.clone();

        let publisher = AdaptivePublisher::new(
            AdaptivePublisherOptions {
                snapshot: Box::new(move || {
                    snapshot_view(&snapshot_state, max_bytes, max_lines, retain)
                }),
                update: Box::new(update_from),
                measure: Box::new(|update| {
                    utf8_byte_length(&serde_json::to_string(update).unwrap_or_default())
                }),
                publish: Box::new(move |update| {
                    let Some(on_update) = on_update.clone() else {
                        return;
                    };
                    let future = on_update(update, publisher_context.clone());
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        let on_error = callback_error.clone();
                        pending_callbacks
                            .lock()
                            .expect("callback tasks poisoned")
                            .push(handle.spawn(async move {
                                let result = future.await;
                                if let Err(message) = &result {
                                    on_error(message.clone());
                                }
                                result
                            }));
                    }
                }),
                on_error: Box::new(move |message| on_error(message)),
                min_interval_ms: Some(OUTPUT_MIN_EMIT_INTERVAL_MS),
                target_bytes_per_second: Some(OUTPUT_TARGET_BYTES_PER_SECOND),
            },
            clock,
        );

        Ok(Self {
            max_bytes,
            max_lines,
            retain,
            context,
            on_update: None,
            state,
            publisher,
            callbacks,
        })
    }

    /// `truncated`.
    pub fn truncated(&self) -> bool {
        let state = self.state.lock().expect("capture state poisoned");
        state.total_bytes > self.max_bytes || total_lines(&state) > self.max_lines
    }

    /// `push(chunk)`.
    pub fn push_bytes(&self, chunk: &[u8]) {
        let text = {
            let mut state = self.state.lock().expect("capture state poisoned");
            if state.disposed {
                return;
            }
            state.pending.extend_from_slice(chunk);
            drain_decoder(&mut state, false)
        };
        self.append_text(&text);
    }

    /// `push(chunk)` for string chunks (the decoder is flushed first, as TS does).
    pub fn push_text(&self, chunk: &str) {
        let pending = {
            let mut state = self.state.lock().expect("capture state poisoned");
            if state.disposed {
                return;
            }
            drain_decoder(&mut state, true)
        };
        self.append_text(&pending);
        self.append_text(chunk);
    }

    /// `finish()`.
    pub fn finish(&self) {
        let pending = {
            let mut state = self.state.lock().expect("capture state poisoned");
            if state.disposed {
                return;
            }
            drain_decoder(&mut state, true)
        };
        self.append_text(&pending);
    }

    /// `setSpillPath(path)`.
    pub fn set_spill_path(&self, path: &str) {
        {
            let mut state = self.state.lock().expect("capture state poisoned");
            if state.disposed || state.spill_path.as_deref() == Some(path) {
                return;
            }
            state.spill_path = Some(path.to_string());
        }
        self.publisher.mark_dirty();
        self.flush();
    }

    /// `snapshot()`.
    pub fn snapshot(&self) -> ShellOutputView {
        let state = self.state.lock().expect("capture state poisoned");
        snapshot_view_of(&state, self.max_bytes, self.max_lines, self.retain)
    }

    /// `flush()`.
    pub fn flush(&self) {
        if self.state.lock().expect("capture state poisoned").disposed {
            return;
        }
        self.publisher.flush(true);
    }

    /// `dispose()`.
    pub fn dispose(&self) {
        self.publisher.dispose();
        self.state.lock().expect("capture state poisoned").disposed = true;
    }

    pub async fn settle_callbacks(&self) -> Result<(), String> {
        let tasks = std::mem::take(&mut *self.callbacks.lock().expect("callback tasks poisoned"));
        let aborts: Vec<_> = tasks
            .iter()
            .map(tokio::task::JoinHandle::abort_handle)
            .collect();
        let settlement = async {
            for task in tasks {
                task.await.map_err(|error| error.to_string())??;
            }
            Ok(())
        };
        match tokio::time::timeout(std::time::Duration::from_millis(5000), settlement).await {
            Ok(result) => {
                if result.is_err() {
                    for task in aborts {
                        task.abort();
                    }
                }
                result
            }
            Err(_) => {
                for task in aborts {
                    task.abort();
                }
                Err("Output callback did not settle within 5000ms".into())
            }
        }
    }

    /// The context this capture publishes against.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// The registered update handler, if any (kept for adapters that re-publish it).
    pub fn on_update(&self) -> Option<&OutputUpdateHandler> {
        self.on_update.as_ref()
    }

    fn append_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let text_bytes = utf8_byte_length(text);
        let guard = self.max_bytes * 2;
        {
            let mut state = self.state.lock().expect("capture state poisoned");
            if state.disposed {
                return;
            }
            state.total_bytes += text_bytes;
            state.newlines += count_newlines(text) as u64;
            state.ends_with_newline = text.ends_with('\n');
            state.current_line_bytes = match text.rfind('\n') {
                None => state.current_line_bytes + text_bytes,
                Some(index) => utf8_byte_length(&text[index + 1..]),
            };
            state.buffer.push_str(text);
            state.buffer_bytes += text_bytes;

            if state.buffer_bytes > guard * 2 {
                state.buffer = if self.retain == ShellOutputRetention::Tail {
                    trim_to_last_utf8_bytes(&state.buffer, guard)
                } else {
                    trim_to_first_utf8_bytes(&state.buffer, guard)
                };
                state.buffer_bytes = utf8_byte_length(&state.buffer);
            }
        }
        self.publisher.mark_dirty();
    }
}

fn total_lines(state: &CaptureState) -> u64 {
    state.newlines
        + if state.ends_with_newline || state.total_bytes == 0 {
            0
        } else {
            1
        }
}

fn snapshot_view(
    state: &Arc<Mutex<CaptureState>>,
    max_bytes: u64,
    max_lines: u64,
    retain: ShellOutputRetention,
) -> ShellOutputView {
    snapshot_view_of(
        &state.lock().expect("capture state poisoned"),
        max_bytes,
        max_lines,
        retain,
    )
}

fn snapshot_view_of(
    state: &CaptureState,
    max_bytes: u64,
    max_lines: u64,
    retain: ShellOutputRetention,
) -> ShellOutputView {
    let options = TruncationOptions {
        max_bytes: Some(max_bytes),
        max_lines: Some(max_lines),
    };
    let retained = if retain == ShellOutputRetention::Head {
        truncate_head(&state.buffer, options)
    } else {
        truncate_tail(&state.buffer, options)
    };
    let total_lines = total_lines(state);
    let truncated = state.total_bytes > max_bytes || total_lines > max_lines;
    let truncation = ShellOutputTruncation {
        truncated,
        truncated_by: if truncated {
            Some(if total_lines > max_lines {
                TruncationLimit::Lines
            } else {
                TruncationLimit::Bytes
            })
        } else {
            None
        },
        total_lines,
        total_bytes: state.total_bytes,
        output_lines: retained.output_lines,
        output_bytes: retained.output_bytes,
        last_line_partial: retained.last_line_partial,
        first_line_exceeds_limit: retained.first_line_exceeds_limit,
        max_lines,
        max_bytes,
    };
    ShellOutputView {
        text: sanitize_shell_output(&retained.content),
        truncation,
        spill_path: state.spill_path.clone(),
        last_line_bytes: if retained.last_line_partial {
            Some(state.current_line_bytes)
        } else {
            None
        },
    }
}

/// `applyShellOutputUpdate(current, update)`.
pub fn apply_shell_output_update(
    current: Option<&ShellOutputView>,
    update: &ShellOutputUpdate,
) -> ShellOutputView {
    match update {
        ShellOutputUpdate::Replace { output } => output.clone(),
        ShellOutputUpdate::Append { text, metadata } => ShellOutputView {
            text: format!(
                "{}{}",
                current.map(|current| current.text.as_str()).unwrap_or(""),
                text
            ),
            truncation: metadata.truncation.clone(),
            spill_path: metadata.spill_path.clone(),
            last_line_bytes: metadata.last_line_bytes,
        },
        ShellOutputUpdate::Slide {
            drop,
            text,
            metadata,
        } => {
            let previous = current.map(|current| current.text.as_str()).unwrap_or("");
            let dropped: String = previous.chars().skip(*drop as usize).collect();
            ShellOutputView {
                text: format!("{dropped}{text}"),
                truncation: metadata.truncation.clone(),
                spill_path: metadata.spill_path.clone(),
                last_line_bytes: metadata.last_line_bytes,
            }
        }
        ShellOutputUpdate::Metadata { metadata } => ShellOutputView {
            text: current
                .map(|current| current.text.clone())
                .unwrap_or_default(),
            truncation: metadata.truncation.clone(),
            spill_path: metadata.spill_path.clone(),
            last_line_bytes: metadata.last_line_bytes,
        },
    }
}

fn metadata_from(current: &ShellOutputView) -> ShellOutputMetadata {
    ShellOutputMetadata {
        truncation: current.truncation.clone(),
        spill_path: current.spill_path.clone(),
        last_line_bytes: current.last_line_bytes,
    }
}

fn update_from(
    previous: Option<&ShellOutputView>,
    current: &ShellOutputView,
) -> Option<ShellOutputUpdate> {
    let Some(previous) = previous else {
        return Some(ShellOutputUpdate::Replace {
            output: current.clone(),
        });
    };
    let metadata = metadata_from(current);
    if current.text == previous.text {
        return Some(ShellOutputUpdate::Metadata { metadata });
    }
    if current.text.len() > previous.text.len() && current.text.starts_with(&previous.text) {
        return Some(ShellOutputUpdate::Append {
            text: current.text[previous.text.len()..].to_string(),
            metadata,
        });
    }
    let scan = (previous.text.chars().count() as u64)
        .min(current.text.chars().count() as u64)
        .min(current.truncation.max_bytes * 2) as usize;
    let shared = suffix_prefix_overlap(&previous.text, &current.text, scan);
    if shared > 0 {
        let drop = previous.text.chars().count() - shared;
        let text: String = current.text.chars().skip(shared).collect();
        return Some(ShellOutputUpdate::Slide {
            drop: drop as u64,
            text,
            metadata,
        });
    }
    Some(ShellOutputUpdate::Replace {
        output: current.clone(),
    })
}

fn suffix_prefix_overlap(before: &str, after: &str, scan: usize) -> usize {
    if before.is_empty() || after.is_empty() || scan == 0 {
        return 0;
    }
    let before_chars: Vec<char> = before.chars().collect();
    let after_chars: Vec<char> = after.chars().collect();
    let tail: Vec<char> = if before_chars.len() > scan {
        before_chars[before_chars.len() - scan..].to_vec()
    } else {
        before_chars.clone()
    };
    for probe_length in [after_chars.len().min(64), 1] {
        let probe: Vec<char> = after_chars[..probe_length].to_vec();
        let mut candidates = 0;
        let mut index = 0;
        while index + probe.len() <= tail.len() {
            if tail[index..index + probe.len()] == probe[..] {
                candidates += 1;
                if candidates > 8 {
                    break;
                }
                let overlap_length = tail.len() - index;
                if overlap_length <= after_chars.len()
                    && tail[index..] == after_chars[..overlap_length]
                {
                    return overlap_length;
                }
            }
            index += 1;
        }
        if probe_length == 1 {
            break;
        }
    }
    0
}

/// `sanitizeShellOutput(text)`: strip control bytes the terminal cannot render.
pub fn sanitize_shell_output(text: &str) -> String {
    text.chars()
        .filter(|character| {
            let code = *character as u32;
            if code <= 0x08 || (0x0b..=0x1f).contains(&code) {
                return false;
            }
            !(0xfff9..=0xfffb).contains(&code)
        })
        .collect()
}

fn count_newlines(text: &str) -> usize {
    text.bytes().filter(|byte| *byte == b'\n').count()
}

fn drain_decoder(state: &mut CaptureState, finish: bool) -> String {
    let pending = std::mem::take(&mut state.pending);
    let mut remaining = pending.as_slice();
    let mut text = String::new();
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                text.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                text.push_str(&String::from_utf8_lossy(&remaining[..valid_up_to]));
                remaining = &remaining[valid_up_to..];
                match error.error_len() {
                    Some(length) => {
                        text.push('\u{fffd}');
                        remaining = &remaining[length..];
                    }
                    None => {
                        if finish {
                            text.push('\u{fffd}');
                        } else {
                            state.pending.extend_from_slice(remaining);
                        }
                        break;
                    }
                }
            }
        }
    }
    // TextDecoder consumes a leading BOM once per decoding stream, including when split
    // across byte chunks. A non-streaming decode resets that state for the next push.
    if state.decoder_at_start && !text.is_empty() {
        if text.starts_with('\u{feff}') {
            text.drain(..'\u{feff}'.len_utf8());
        }
        state.decoder_at_start = false;
    }
    if finish {
        state.decoder_at_start = true;
    }
    text
}

fn trim_to_last_utf8_bytes(text: &str, max_bytes: u64) -> String {
    let bytes = text.as_bytes();
    if bytes.len() as u64 <= max_bytes {
        return text.to_string();
    }
    let mut start = bytes.len() - max_bytes as usize;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn trim_to_first_utf8_bytes(text: &str, max_bytes: u64) -> String {
    let bytes = text.as_bytes();
    if bytes.len() as u64 <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes as usize;
    while end > 0 && (bytes[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Helper so `TruncatedBy` stays referenced from this module's API surface.
pub type CaptureTruncatedBy = TruncatedBy;

#[cfg(test)]
mod tests {
    use super::{OutputCapture, OutputCaptureHandlers};
    use crate::harness::context::background_context;
    use std::sync::Arc;

    #[tokio::test]
    async fn callback_settlement_waits_for_observer_completion() {
        // Given: an observer waiting for a registered completion signal.
        let (release, released) = tokio::sync::oneshot::channel();
        let released = Arc::new(std::sync::Mutex::new(Some(released)));
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = completed.clone();
        let capture = OutputCapture::new(
            None,
            background_context(),
            OutputCaptureHandlers {
                on_update: Some(Arc::new(move |_, _| {
                    let released = released.lock().expect("receiver lock").take();
                    let observed = observed.clone();
                    Box::pin(async move {
                        if let Some(released) = released {
                            released.await.expect("release observer");
                            observed.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        Ok(())
                    })
                })),
                on_error: Arc::new(|message| panic!("{message}")),
            },
        )
        .expect("capture");
        capture.push_text("output");
        capture.flush();
        // When: the observer is released and execution awaits settlement.
        release.send(()).expect("release signal");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            capture.settle_callbacks(),
        )
        .await
        .expect("bounded settlement")
        .expect("observer settled");
        // Then: settlement includes the observer's side effect.
        assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
        capture.dispose();
    }

    #[tokio::test(start_paused = true)]
    async fn nonsettling_observer_returns_callback_timeout() {
        // Given: an observer that never resolves, with virtual Tokio time.
        let capture = OutputCapture::new(
            None,
            background_context(),
            OutputCaptureHandlers {
                on_update: Some(Arc::new(|_, _| Box::pin(std::future::pending()))),
                on_error: Arc::new(|message| panic!("{message}")),
            },
        )
        .expect("capture");
        capture.push_text("output");
        capture.flush();
        // When: callback settlement reaches its deadline.
        let result = capture.settle_callbacks().await;
        // Then: it fails rather than blocking execution forever.
        assert!(result.is_err());
        capture.dispose();
    }

    #[tokio::test]
    async fn observer_rejection_is_reported_before_settlement() {
        let (errors, mut received) = tokio::sync::mpsc::unbounded_channel();
        let capture = OutputCapture::new(
            None,
            background_context(),
            OutputCaptureHandlers {
                on_update: Some(Arc::new(|_, _| Box::pin(async { Err("rejected".into()) }))),
                on_error: Arc::new(move |message| errors.send(message).expect("error receiver")),
            },
        )
        .expect("capture");
        capture.push_text("output");
        capture.flush();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), received.recv())
                .await
                .expect("rejection event"),
            Some("rejected".into())
        );
        assert_eq!(capture.settle_callbacks().await, Err("rejected".into()));
        capture.dispose();
    }

    fn capture() -> OutputCapture {
        OutputCapture::new(
            None,
            background_context(),
            OutputCaptureHandlers {
                on_update: None,
                on_error: Arc::new(|message| panic!("{message}")),
            },
        )
        .expect("capture constructs")
    }

    #[tokio::test]
    async fn invalid_utf8_is_replaced_without_losing_following_output() {
        let capture = capture();

        capture.push_bytes(b"a\xffb\xe2\x82c\xf0\x80\x80\x80d");

        let output = capture.snapshot();
        assert_eq!(
            output.text,
            "a\u{fffd}b\u{fffd}c\u{fffd}\u{fffd}\u{fffd}\u{fffd}d"
        );
        assert_eq!(output.truncation.total_bytes, 22);
        capture.dispose();
    }

    #[tokio::test]
    async fn split_utf8_is_retained_until_completed() {
        let capture = capture();
        capture.push_bytes(b"a\xf0\x9f");
        assert_eq!(capture.snapshot().text, "a");

        capture.push_bytes(b"\x98\x80b");

        assert_eq!(capture.snapshot().text, "a\u{1f600}b");
        capture.dispose();
    }

    #[tokio::test]
    async fn string_input_flushes_and_resets_incomplete_byte_input() {
        let capture = capture();
        capture.push_bytes(b"a\xe2\x82");

        capture.push_text("b");

        assert_eq!(capture.snapshot().text, "a\u{fffd}b");
        capture.push_bytes(b"\xacc");
        assert_eq!(capture.snapshot().text, "a\u{fffd}b\u{fffd}c");
        capture.dispose();
    }

    #[tokio::test]
    async fn finish_replaces_incomplete_input_only_once() {
        let capture = capture();
        capture.push_bytes(b"a\xf0\x9f\x98");

        capture.finish();
        capture.finish();

        let output = capture.snapshot();
        assert_eq!(output.text, "a\u{fffd}");
        assert_eq!(output.truncation.total_bytes, 4);
        capture.dispose();
    }

    #[tokio::test]
    async fn decoder_consumes_split_leading_bom_and_resets_after_finish() {
        let capture = capture();
        capture.push_bytes(b"\xef");
        capture.push_bytes(b"\xbb\xbfa\xef\xbb\xbf");
        capture.finish();

        capture.push_bytes(b"\xef\xbb\xbfb");

        assert_eq!(capture.snapshot().text, "a\u{feff}b");
        capture.dispose();
    }
}
