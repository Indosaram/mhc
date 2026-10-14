//! Port of senpi `packages/tui/native/linux/src/linux-platform-x11.c` (+ `clipboard-worker.h`),
//! the Linux/X11 native clipboard helper: reads the `CLIPBOARD` selection over the X protocol
//! (TARGETS negotiation, then the preferred text or image target, including INCR transfers) with
//! one shared two-second deadline. senpi's addon talks XCB from C; this port uses x11rb's pure-Rust
//! connection. Registered by [`crate::native_platform::register_builtin_native_platform_helper`]
//! and loaded lazily by [`crate::native_platform::get_native_clipboard`] when `DISPLAY` is set.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropertyNotifyEvent, SelectionNotifyEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::native_platform::{NativeClipboard, NativePlatformHelper};

const MAX_CLIPBOARD_BYTES: usize = 50 * 1024 * 1024;
const CLIPBOARD_TIMEOUT_MS: u64 = 2000;
/// `clipboard-worker.h`: allow the two-second transfer deadline to report its own error first.
const WORKER_TIMEOUT_MS: u64 = 3000;

const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain;charset=UTF-8", "UTF8_STRING", "text/plain", "STRING"];
const IMAGE_TYPES: [&str; 6] = ["image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp", "image/tiff"];

pub struct LinuxX11PlatformHelper;

#[derive(Default)]
struct PropertyData {
    data: Vec<u8>,
    items: u32,
    format: u8,
    type_: Atom,
}

struct X11Clipboard {
    connection: RustConnection,
    window: Window,
    clipboard: Atom,
    property: Atom,
    targets: Atom,
    incr: Atom,
    deadline: Instant,
}

/// The clipboard contents as senpi's `clipboard_format` distinguishes them.
enum ClipboardContents {
    Empty,
    Buffer(Vec<u8>),
    Latin1(Vec<u8>),
    Utf8(Vec<u8>),
}

fn append_bytes(result: &mut PropertyData, bytes: &[u8]) -> bool {
    if bytes.len() > MAX_CLIPBOARD_BYTES - result.data.len() {
        return false;
    }
    result.data.extend_from_slice(bytes);
    true
}

impl X11Clipboard {
    /// `wait_for_input`: wait for the connection to become readable within the shared deadline.
    fn wait_for_input(&self) -> bool {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let Ok(timeout) = rustix::event::Timespec::try_from(remaining) else { return false };
            let stream = self.connection.stream();
            let mut descriptors = [rustix::event::PollFd::new(stream, rustix::event::PollFlags::IN)];
            match rustix::event::poll(&mut descriptors, Some(&timeout)) {
                Ok(ready) => return ready > 0 && descriptors[0].revents().contains(rustix::event::PollFlags::IN),
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return false,
            }
        }
    }

    /// `wait_for_event`: the next event of the wanted kind before the deadline.
    fn wait_for_event<T>(&self, select: impl Fn(Event) -> Option<T>) -> Option<T> {
        self.connection.flush().ok()?;
        while Instant::now() < self.deadline {
            match self.connection.poll_for_event() {
                Ok(Some(event)) => {
                    if let Some(event) = select(event) {
                        return Some(event);
                    }
                }
                Ok(None) => {
                    if !self.wait_for_input() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        None
    }

    fn read_property(&self, remove: bool, result: &mut PropertyData) -> bool {
        let max_items = u32::try_from(MAX_CLIPBOARD_BYTES / 4).unwrap_or(u32::MAX);
        let Ok(cookie) = self.connection.get_property(remove, self.window, self.property, AtomEnum::ANY, 0, max_items) else { return false };
        let Ok(reply) = cookie.reply() else { return false };
        let valid_format = matches!(reply.format, 8 | 16 | 32);
        let length = u64::from(reply.value_len) * u64::from(reply.format / 8);
        if reply.bytes_after == 0
            && reply.type_ != u32::from(AtomEnum::NONE)
            && valid_format
            && length <= MAX_CLIPBOARD_BYTES as u64
            && length <= reply.value.len() as u64
        {
            result.type_ = reply.type_;
            result.format = reply.format;
            result.items = reply.value_len;
            return append_bytes(result, &reply.value[..usize::try_from(length).unwrap_or(0)]);
        }
        false
    }

    fn request_selection(&self, target: Atom, result: &mut PropertyData) -> bool {
        *result = PropertyData::default();
        if self.connection.delete_property(self.window, self.property).is_err()
            || self.connection.convert_selection(self.window, self.clipboard, target, self.property, x11rb::CURRENT_TIME).is_err()
        {
            return false;
        }
        loop {
            let Some(event) = self.wait_for_event(|event| match event {
                Event::SelectionNotify(event) => Some(event),
                _ => None,
            }) else {
                return false;
            };
            let SelectionNotifyEvent { selection, target: event_target, property, .. } = event;
            if selection != self.clipboard || event_target != target {
                continue;
            }
            // No owner, or the owner does not offer this target.
            if property == u32::from(AtomEnum::NONE) {
                return true;
            }
            if !self.read_property(false, result) {
                return false;
            }
            break;
        }
        if result.type_ != self.incr {
            return true;
        }

        *result = PropertyData::default();
        if self.connection.delete_property(self.window, self.property).is_err() {
            return false;
        }
        loop {
            let Some(event) = self.wait_for_event(|event| match event {
                Event::PropertyNotify(event) => Some(event),
                _ => None,
            }) else {
                return false;
            };
            let PropertyNotifyEvent { atom, state, .. } = event;
            if atom != self.property || state != x11rb::protocol::xproto::Property::NEW_VALUE {
                continue;
            }
            let mut chunk = PropertyData::default();
            if !self.read_property(true, &mut chunk) {
                return false;
            }
            if !append_property(result, &chunk) {
                return false;
            }
            if chunk.items == 0 {
                return true;
            }
        }
    }

    fn intern_atom(&self, name: &str) -> Atom {
        self.connection
            .intern_atom(false, name.as_bytes())
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or(u32::from(AtomEnum::NONE), |reply| reply.atom)
    }

    fn open() -> Option<Self> {
        let deadline = Instant::now() + Duration::from_millis(CLIPBOARD_TIMEOUT_MS);
        let (connection, screen_number) = RustConnection::connect(None).ok()?;
        let root = connection.setup().roots.get(screen_number)?.root;
        let window = connection.generate_id().ok()?;
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .ok()?;
        let mut clipboard = Self { connection, window, clipboard: 0, property: 0, targets: 0, incr: 0, deadline };
        clipboard.clipboard = clipboard.intern_atom("CLIPBOARD");
        clipboard.property = clipboard.intern_atom("PI_CLIPBOARD");
        clipboard.targets = clipboard.intern_atom("TARGETS");
        clipboard.incr = clipboard.intern_atom("INCR");
        (clipboard.clipboard != 0 && clipboard.property != 0 && clipboard.targets != 0 && clipboard.incr != 0).then_some(clipboard)
    }

    /// `preferred_target`: `Some(None)` when the owner offers none of the wanted types.
    fn preferred_target(&self, image: bool) -> Option<Option<Atom>> {
        let types: &[&str] = if image { &IMAGE_TYPES } else { &TEXT_TYPES };
        let mut wanted = Vec::with_capacity(types.len());
        for name in types {
            let atom = self.intern_atom(name);
            if atom == u32::from(AtomEnum::NONE) {
                return None;
            }
            wanted.push(atom);
        }

        let mut targets = PropertyData::default();
        let received = self.request_selection(self.targets, &mut targets);
        let atom_size = std::mem::size_of::<Atom>();
        let valid = targets.type_ == u32::from(AtomEnum::ATOM)
            && targets.format == 32
            && targets.data.len() % atom_size == 0
            && targets.items as usize == targets.data.len() / atom_size;
        if received && valid {
            let offered: Vec<Atom> = targets
                .data
                .chunks_exact(atom_size)
                .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .collect();
            if let Some(atom) = wanted.iter().find(|atom| offered.contains(atom)) {
                return Some(Some(*atom));
            }
        }
        // Some owners refuse TARGETS but still serve text directly.
        if received && !image && targets.type_ == u32::from(AtomEnum::NONE) {
            let atom = self.intern_atom("UTF8_STRING");
            return (atom != u32::from(AtomEnum::NONE)).then_some(Some(atom));
        }
        (received && (valid || targets.type_ == u32::from(AtomEnum::NONE))).then_some(None)
    }

    fn read_selection(&self, image: bool) -> Option<PropertyData> {
        let target = self.preferred_target(image)?;
        let mut result = PropertyData::default();
        match target {
            None => Some(result),
            Some(target) => self.request_selection(target, &mut result).then_some(result),
        }
    }
}

fn append_property(result: &mut PropertyData, chunk: &PropertyData) -> bool {
    if !matches!(chunk.format, 8 | 16 | 32)
        || chunk.type_ == u32::from(AtomEnum::NONE)
        || u64::from(chunk.items) * u64::from(chunk.format / 8) != chunk.data.len() as u64
        || (result.type_ != u32::from(AtomEnum::NONE) && (result.type_ != chunk.type_ || result.format != chunk.format))
        || chunk.items > u32::MAX - result.items
    {
        return false;
    }
    if !append_bytes(result, &chunk.data) {
        return false;
    }
    result.type_ = chunk.type_;
    result.format = chunk.format;
    result.items += chunk.items;
    true
}

fn read_clipboard(image: bool) -> Result<ClipboardContents, String> {
    let error = || "Could not read X11 clipboard".to_owned();
    let clipboard = X11Clipboard::open().ok_or_else(error)?;
    let contents = clipboard.read_selection(image).ok_or_else(error)?;
    Ok(if contents.data.is_empty() {
        ClipboardContents::Empty
    } else if image {
        ClipboardContents::Buffer(contents.data)
    } else if contents.type_ == u32::from(AtomEnum::STRING) {
        ClipboardContents::Latin1(contents.data)
    } else {
        ClipboardContents::Utf8(contents.data)
    })
}

static WORKER_BUSY: AtomicBool = AtomicBool::new(false);

/// `clipboard_execute`: one private reader thread at a time with a bounded wait; other callers,
/// and a caller whose wait expires, get "unavailable". A late result is discarded.
fn clipboard_execute(image: bool) -> Option<Result<ClipboardContents, String>> {
    if WORKER_BUSY.swap(true, Ordering::SeqCst) {
        return None;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("x11-clipboard".into()).spawn(move || {
        let result = read_clipboard(image);
        WORKER_BUSY.store(false, Ordering::SeqCst);
        let _ = sender.send(result);
    });
    if spawned.is_err() {
        WORKER_BUSY.store(false, Ordering::SeqCst);
        return None;
    }
    receiver.recv_timeout(Duration::from_millis(WORKER_TIMEOUT_MS)).ok()
}

impl NativeClipboard for LinuxX11PlatformHelper {
    fn get_text(&self) -> Result<Option<String>, String> {
        match clipboard_execute(false).transpose()? {
            None | Some(ClipboardContents::Empty) => Ok(None),
            Some(ClipboardContents::Latin1(bytes)) => Ok(Some(bytes.into_iter().map(char::from).collect())),
            Some(ClipboardContents::Utf8(bytes) | ClipboardContents::Buffer(bytes)) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        }
    }

    fn get_image(&self) -> Result<Option<Vec<u8>>, String> {
        match clipboard_execute(true).transpose()? {
            None | Some(ClipboardContents::Empty) => Ok(None),
            Some(ClipboardContents::Buffer(bytes) | ClipboardContents::Latin1(bytes) | ClipboardContents::Utf8(bytes)) => Ok(Some(bytes)),
        }
    }
}

impl NativePlatformHelper for LinuxX11PlatformHelper {}
