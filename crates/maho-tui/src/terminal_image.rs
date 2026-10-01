//! Port of senpi `packages/tui/src/terminal-image.ts`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, RwLock};

use crate::process_env::{self, Env};
use crate::terminal_capabilities::{
    self, detect_terminal_capabilities_in, CellDimensions, DetectedTerminalCapabilities,
    ImageProtocol,
};
use crate::terminal_text::{sanitize_terminal_label, shorten_image_path, ImageDimensions};
use crate::tmux_cursor_query::{exec_file_sync, TmuxExecFile};
use crate::tmux_image_capability::{
    decide_tmux_image_capability, parse_tmux_kitty_terminal_override, TmuxKittyPlacement,
    TmuxKittyTerminalOverride, TmuxImageDecision,
};
use crate::tmux_image_probe::{probe_tmux_image_state, TmuxAllowPassthrough};

const PROBE_TIMEOUT_MS: u64 = 250;
const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPassthroughState {
    pub allow_passthrough: TmuxAllowPassthrough,
    pub client_termname: String,
    pub cell_width_px: Option<u32>,
    pub cell_height_px: Option<u32>,
}

impl Default for TmuxPassthroughState {
    fn default() -> Self {
        Self {
            allow_passthrough: TmuxAllowPassthrough::Off,
            client_termname: String::new(),
            cell_width_px: None,
            cell_height_px: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
    pub tmux_passthrough: bool,
    pub kitty_unicode_placeholders: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilityOverrides {
    pub images: Option<Option<ImageProtocol>>,
    pub true_color: Option<bool>,
    pub hyperlinks: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: bool,
    pub image_id: Option<u32>,
    pub move_cursor: bool,
}

impl Default for ImageRenderOptions {
    fn default() -> Self {
        Self {
            max_width_cells: None,
            max_height_cells: None,
            preserve_aspect_ratio: true,
            image_id: None,
            move_cursor: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCellSize {
    pub columns: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRenderResult {
    pub sequence: String,
    pub columns: u32,
    pub rows: u32,
    pub image_id: Option<u32>,
    pub lines: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyImageMetadata {
    pub image_id: u32,
    pub columns: u32,
    pub rows: u32,
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImagePlacement {
    pub image_id: u32,
    pub transmission_generation: u64,
    pub transmission_bytes: u64,
    pub estimated_decoded_bytes: u64,
    pub sequence: String,
    pub replacement_line: String,
}

#[derive(Debug, Clone, Copy)]
struct RegisteredKittyImageMetadata {
    metadata: KittyImageMetadata,
    transmission_generation: u64,
}

static CAPABILITY_STATE: LazyLock<RwLock<CapabilityCache>> =
    LazyLock::new(|| RwLock::new(CapabilityCache::default()));
static CELL_DIMENSIONS: LazyLock<RwLock<CellDimensions>> = LazyLock::new(|| {
    RwLock::new(CellDimensions {
        width_px: 9,
        height_px: 18,
    })
});
static KITTY_IMAGE_METADATA: LazyLock<Mutex<KittyRegistry>> =
    LazyLock::new(|| Mutex::new(KittyRegistry::default()));
static IMAGE_ID_COUNTER: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(seed_image_id_state()));

#[derive(Default)]
struct CapabilityCache {
    cached: Option<TerminalCapabilities>,
    overrides: CapabilityOverrides,
}

#[derive(Default)]
struct KittyRegistry {
    entries: HashMap<u32, RegisteredKittyImageMetadata>,
    generation: u64,
    order: Vec<u32>,
}

fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn mutex_lock<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn get_cell_dimensions() -> CellDimensions {
    *read_lock(&CELL_DIMENSIONS)
}

pub fn set_cell_dimensions(dims: CellDimensions) {
    *write_lock(&CELL_DIMENSIONS) = dims;
}

fn seed_image_id_state() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        | 1
}

pub fn allocate_image_id() -> u32 {
    let mut state = mutex_lock(&IMAGE_ID_COUNTER);
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x % 0xffff_fffe) as u32 + 1
}

fn probe_tmux_hyperlinks() -> bool {
    let args = vec![
        "display-message".to_string(),
        "-p".to_string(),
        "#{client_termfeatures}".to_string(),
    ];
    match exec_file_sync("tmux", &args, PROBE_TIMEOUT_MS) {
        Ok(termfeatures) => termfeatures
            .split(',')
            .map(str::trim)
            .any(|feature| feature == "hyperlinks"),
        Err(_) => false,
    }
}

fn detect_capabilities_from_environment(
    env: &Env,
    platform: &str,
    tmux_forwards_hyperlink: &dyn Fn() -> bool,
) -> DetectedTerminalCapabilities {
    let term_program = env
        .get("TERM_PROGRAM")
        .map(|v| v.to_lowercase())
        .unwrap_or_default();
    let terminal_emulator = env
        .get("TERMINAL_EMULATOR")
        .map(|v| v.to_lowercase())
        .unwrap_or_default();
    let term = env.get("TERM").map(|v| v.to_lowercase()).unwrap_or_default();
    let color_term = env
        .get("COLORTERM")
        .map(|v| v.to_lowercase())
        .unwrap_or_default();
    let has_true_color_hint = color_term == "truecolor" || color_term == "24bit";
    let is_windows_console = platform == "win32";

    let plain = |images: Option<ImageProtocol>, true_color: bool, hyperlinks: bool| {
        DetectedTerminalCapabilities {
            images,
            true_color,
            hyperlinks,
            tmux_passthrough: None,
            kitty_unicode_placeholders: None,
            cell_dimensions: None,
        }
    };

    if process_env::truthy(env, "TMUX") || term.starts_with("tmux") {
        return plain(None, has_true_color_hint, tmux_forwards_hyperlink());
    }

    if term.starts_with("screen") {
        return plain(None, has_true_color_hint, false);
    }

    if process_env::truthy(env, "KITTY_WINDOW_ID") || term_program == "kitty" {
        return plain(Some(ImageProtocol::Kitty), true, true);
    }

    if term_program == "ghostty"
        || term.contains("ghostty")
        || process_env::truthy(env, "GHOSTTY_RESOURCES_DIR")
    {
        return plain(Some(ImageProtocol::Kitty), true, true);
    }

    if process_env::truthy(env, "WEZTERM_PANE") || term_program == "wezterm" {
        return plain(Some(ImageProtocol::Kitty), true, true);
    }

    if term_program == "warpterminal"
        || process_env::truthy(env, "WARP_SESSION_ID")
        || process_env::truthy(env, "WARP_TERMINAL_SESSION_UUID")
    {
        return plain(Some(ImageProtocol::Kitty), true, true);
    }

    if process_env::truthy(env, "ITERM_SESSION_ID") || term_program == "iterm.app" {
        return plain(Some(ImageProtocol::Iterm2), true, true);
    }

    if process_env::truthy(env, "WT_SESSION") {
        return plain(None, true, true);
    }

    if term_program == "alacritty" || term_program == "vscode" || term_program == "zed" {
        return plain(None, true, true);
    }

    if terminal_emulator == "jetbrains-jediterm" {
        return plain(None, true, false);
    }

    if is_windows_console {
        return plain(None, true, false);
    }

    plain(None, has_true_color_hint, false)
}

fn parse_boolean_capability_override(value: Option<&str>) -> Option<bool> {
    match value {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

fn tmux_image_detector(env: &Env, exec_tmux: Option<&TmuxExecFile>) -> terminal_capabilities::TmuxImageDetection {
    let state = probe_tmux_image_state(env, exec_tmux);
    let override_terminal: TmuxKittyTerminalOverride =
        parse_tmux_kitty_terminal_override(env.get("PI_TUI_TMUX_KITTY_TERMINAL").map(String::as_str));
    let decision = decide_tmux_image_capability(&state, override_terminal);
    let (enabled, placeholder) = match decision {
        TmuxImageDecision::Enabled { placement, .. } => {
            (true, placement == TmuxKittyPlacement::Placeholder)
        }
        TmuxImageDecision::Disabled { .. } => (false, false),
    };
    terminal_capabilities::TmuxImageDetection {
        enabled,
        placeholder,
        hyperlinks: state.base().hyperlinks,
        cell_dimensions: state.base().cell_dimensions,
    }
}

static TMUX_DETECTOR_INSTALLED: std::sync::Once = std::sync::Once::new();

pub fn install_tmux_image_detector() {
    TMUX_DETECTOR_INSTALLED.call_once(|| {
        terminal_capabilities::set_tmux_image_detector(tmux_image_detector);
    });
}

pub fn detect_capabilities() -> DetectedTerminalCapabilities {
    let env = process_env::current();
    detect_capabilities_in(
        &env,
        crate::native_platform::node_platform(),
        probe_tmux_hyperlinks,
    )
}

pub fn detect_capabilities_in(
    env: &Env,
    platform: &str,
    tmux_hyperlink_probe: fn() -> bool,
) -> DetectedTerminalCapabilities {
    let hyperlink_override =
        parse_boolean_capability_override(env.get("PI_HYPERLINKS").map(String::as_str));
    let detected = if let Some(value) = hyperlink_override {
        let probe = move || value;
        detect_capabilities_from_environment(env, platform, &probe)
    } else {
        detect_capabilities_from_environment(env, platform, &tmux_hyperlink_probe)
    };
    merge_env_overrides(env, detected)
}

fn merge_env_overrides(env: &Env, detected: DetectedTerminalCapabilities) -> DetectedTerminalCapabilities {
    let hyperlinks = parse_boolean_capability_override(env.get("PI_HYPERLINKS").map(String::as_str));
    let image_protocol = env.get("PI_IMAGE_PROTOCOL").map(|v| v.to_lowercase());
    let images = match image_protocol.as_deref() {
        Some("kitty") => Some(Some(ImageProtocol::Kitty)),
        Some("iterm2") => Some(Some(ImageProtocol::Iterm2)),
        Some("none") | Some("0") => Some(None),
        _ => None,
    };
    let true_color = parse_boolean_capability_override(env.get("PI_TRUE_COLOR").map(String::as_str));

    DetectedTerminalCapabilities {
        images: match images {
            Some(images) => images,
            None => detected.images,
        },
        true_color: true_color.unwrap_or(detected.true_color),
        hyperlinks: hyperlinks.unwrap_or(detected.hyperlinks),
        tmux_passthrough: detected.tmux_passthrough,
        kitty_unicode_placeholders: detected.kitty_unicode_placeholders,
        cell_dimensions: detected.cell_dimensions,
    }
}

pub fn detect_capabilities_with_tmux(
    env: &Env,
    platform: &str,
    tmux_forwards_hyperlink: &dyn Fn() -> bool,
    tmux_passthrough_state: &dyn Fn() -> TmuxPassthroughState,
) -> DetectedTerminalCapabilities {
    install_tmux_image_detector();
    let legacy = tmux_passthrough_state();
    let hyperlinks = tmux_forwards_hyperlink();
    let mut merged = env.clone();
    if merged.get("TMUX").is_none_or(|v| v.is_empty()) {
        merged.insert("TMUX".to_string(), "compat,0,0".to_string());
    }
    let legacy_output = [
        "3.4".to_string(),
        allow_passthrough_value(legacy.allow_passthrough).to_string(),
        "on".to_string(),
        "1".to_string(),
        "1".to_string(),
        "1".to_string(),
        legacy.client_termname.clone(),
        legacy.cell_width_px.map(|v| v.to_string()).unwrap_or_default(),
        legacy
            .cell_height_px
            .map(|v| v.to_string())
            .unwrap_or_default(),
        if hyperlinks { "hyperlinks".to_string() } else { String::new() },
    ]
    .join("|");

    let exec: TmuxExecFile = std::sync::Arc::new(move |_file: &str, _args: &[String]| {
        Ok(legacy_output.clone())
    });
    let detected = detect_terminal_capabilities_in(&merged, platform, Some(&exec));
    if let Some(dims) = detected.cell_dimensions {
        set_cell_dimensions(dims);
    }
    DetectedTerminalCapabilities {
        hyperlinks,
        ..detected
    }
}

fn allow_passthrough_value(value: TmuxAllowPassthrough) -> &'static str {
    match value {
        TmuxAllowPassthrough::Off => "off",
        TmuxAllowPassthrough::On => "on",
        TmuxAllowPassthrough::All => "all",
    }
}

fn to_capabilities(detected: DetectedTerminalCapabilities) -> TerminalCapabilities {
    TerminalCapabilities {
        images: detected.images,
        true_color: detected.true_color,
        hyperlinks: detected.hyperlinks,
        tmux_passthrough: detected.tmux_passthrough.unwrap_or(false),
        kitty_unicode_placeholders: detected.kitty_unicode_placeholders.unwrap_or(false),
    }
}

pub fn get_capabilities() -> TerminalCapabilities {
    install_tmux_image_detector();
    if let Some(cached) = read_lock(&CAPABILITY_STATE).cached {
        return cached;
    }
    let overrides = read_lock(&CAPABILITY_STATE).overrides;
    let mut resolved = to_capabilities(detect_capabilities());
    if let Some(images) = overrides.images {
        resolved.images = images;
    }
    if let Some(true_color) = overrides.true_color {
        resolved.true_color = true_color;
    }
    if let Some(hyperlinks) = overrides.hyperlinks {
        resolved.hyperlinks = hyperlinks;
    }
    write_lock(&CAPABILITY_STATE).cached = Some(resolved);
    resolved
}

pub fn reset_capabilities_cache() {
    write_lock(&CAPABILITY_STATE).cached = None;
}

pub fn set_capability_overrides(overrides: CapabilityOverrides) {
    let mut state = write_lock(&CAPABILITY_STATE);
    if state.overrides.images == overrides.images
        && state.overrides.true_color == overrides.true_color
        && state.overrides.hyperlinks == overrides.hyperlinks
    {
        return;
    }
    state.overrides = overrides;
    state.cached = None;
}

pub fn set_capabilities(caps: TerminalCapabilities) {
    write_lock(&CAPABILITY_STATE).cached = Some(caps);
}

pub fn wrap_tmux_passthrough(sequence: &str) -> String {
    format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
}

pub fn is_image_line(line: &str) -> bool {
    if line.starts_with(KITTY_PREFIX) || line.starts_with(ITERM2_PREFIX) {
        return true;
    }
    line.contains(KITTY_PREFIX) || line.contains(ITERM2_PREFIX)
}

pub fn encode_kitty(base64_data: &str, options: &KittyEncodeOptions) -> String {
    const CHUNK_SIZE: usize = 4096;

    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];
    if options.virtual_placement {
        params.push("U=1".to_string());
    } else if !options.move_cursor {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns.filter(|v| *v != 0) {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = options.rows.filter(|v| *v != 0) {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = options.image_id.filter(|v| *v != 0) {
        params.push(format!("i={image_id}"));
    }
    let params = params.join(",");

    let mut chunks: Vec<String> = Vec::new();
    if base64_data.len() <= CHUNK_SIZE {
        chunks.push(format!("\x1b_G{params};{base64_data}\x1b\\"));
    } else {
        let mut offset = 0usize;
        let mut is_first = true;
        while offset < base64_data.len() {
            let end = (offset + CHUNK_SIZE).min(base64_data.len());
            let chunk = &base64_data[offset..end];
            let is_last = offset + CHUNK_SIZE >= base64_data.len();
            if is_first {
                chunks.push(format!("\x1b_G{params},m=1;{chunk}\x1b\\"));
                is_first = false;
            } else if is_last {
                chunks.push(format!("\x1b_Gm=0;{chunk}\x1b\\"));
            } else {
                chunks.push(format!("\x1b_Gm=1;{chunk}\x1b\\"));
            }
            offset += CHUNK_SIZE;
        }
    }

    if get_capabilities().tmux_passthrough {
        return chunks
            .iter()
            .map(|chunk| wrap_tmux_passthrough(chunk))
            .collect();
    }
    chunks.concat()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyEncodeOptions {
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub image_id: Option<u32>,
    pub move_cursor: bool,
    pub virtual_placement: bool,
}

const KITTY_PLACEHOLDER: char = '\u{10eeee}';

const KITTY_PLACEHOLDER_DIACRITICS: [u32; 297] = [
    0x0305, 0x030d, 0x030e, 0x0310, 0x0312, 0x033d, 0x033e, 0x033f, 0x0346, 0x034a, 0x034b, 0x034c,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035b, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036a, 0x036b, 0x036c, 0x036d, 0x036e, 0x036f, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059c, 0x059d, 0x059e, 0x059f, 0x05a0, 0x05a1,
    0x05a8, 0x05a9, 0x05ab, 0x05ac, 0x05af, 0x05c4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065a, 0x065b, 0x065d, 0x065e, 0x06d6, 0x06d7, 0x06d8,
    0x06d9, 0x06da, 0x06db, 0x06dc, 0x06df, 0x06e0, 0x06e1, 0x06e2, 0x06e4, 0x06e7, 0x06e8, 0x06eb,
    0x06ec, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073a, 0x073d, 0x073f, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074a, 0x07eb, 0x07ec, 0x07ed, 0x07ee, 0x07ef, 0x07f0, 0x07f1, 0x07f3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081b, 0x081c, 0x081d, 0x081e, 0x081f, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082a, 0x082b, 0x082c, 0x082d, 0x0951, 0x0953, 0x0954,
    0x0f82, 0x0f83, 0x0f86, 0x0f87, 0x135d, 0x135e, 0x135f, 0x17dd, 0x193a, 0x1a17, 0x1a75, 0x1a76,
    0x1a77, 0x1a78, 0x1a79, 0x1a7a, 0x1a7b, 0x1a7c, 0x1b6b, 0x1b6d, 0x1b6e, 0x1b6f, 0x1b70, 0x1b71,
    0x1b72, 0x1b73, 0x1cd0, 0x1cd1, 0x1cd2, 0x1cda, 0x1cdb, 0x1ce0, 0x1dc0, 0x1dc1, 0x1dc3, 0x1dc4,
    0x1dc5, 0x1dc6, 0x1dc7, 0x1dc8, 0x1dc9, 0x1dcb, 0x1dcc, 0x1dd1, 0x1dd2, 0x1dd3, 0x1dd4, 0x1dd5,
    0x1dd6, 0x1dd7, 0x1dd8, 0x1dd9, 0x1dda, 0x1ddb, 0x1ddc, 0x1ddd, 0x1dde, 0x1ddf, 0x1de0, 0x1de1,
    0x1de2, 0x1de3, 0x1de4, 0x1de5, 0x1de6, 0x1dfe, 0x20d0, 0x20d1, 0x20d4, 0x20d5, 0x20d6, 0x20d7,
    0x20db, 0x20dc, 0x20e1, 0x20e7, 0x20e9, 0x20f0, 0x2cef, 0x2cf0, 0x2cf1, 0x2de0, 0x2de1, 0x2de2,
    0x2de3, 0x2de4, 0x2de5, 0x2de6, 0x2de7, 0x2de8, 0x2de9, 0x2dea, 0x2deb, 0x2dec, 0x2ded, 0x2dee,
    0x2def, 0x2df0, 0x2df1, 0x2df2, 0x2df3, 0x2df4, 0x2df5, 0x2df6, 0x2df7, 0x2df8, 0x2df9, 0x2dfa,
    0x2dfb, 0x2dfc, 0x2dfd, 0x2dfe, 0x2dff, 0xa66f, 0xa67c, 0xa67d, 0xa6f0, 0xa6f1, 0xa8e0, 0xa8e1,
    0xa8e2, 0xa8e3, 0xa8e4, 0xa8e5, 0xa8e6, 0xa8e7, 0xa8e8, 0xa8e9, 0xa8ea, 0xa8eb, 0xa8ec, 0xa8ed,
    0xa8ee, 0xa8ef, 0xa8f0, 0xa8f1, 0xaab0, 0xaab2, 0xaab3, 0xaab7, 0xaab8, 0xaabe, 0xaabf, 0xaac1,
    0xfe20, 0xfe21, 0xfe22, 0xfe23, 0xfe24, 0xfe25, 0xfe26, 0x10a0f, 0x10a38, 0x1d185, 0x1d186,
    0x1d187, 0x1d188, 0x1d189, 0x1d1aa, 0x1d1ab, 0x1d1ac, 0x1d1ad, 0x1d242, 0x1d243, 0x1d244,
];

pub const KITTY_PLACEHOLDER_MAX: usize = KITTY_PLACEHOLDER_DIACRITICS.len();

fn diacritic(index: usize) -> char {
    let clamped = index.min(KITTY_PLACEHOLDER_MAX - 1);
    char::from_u32(KITTY_PLACEHOLDER_DIACRITICS[clamped]).unwrap_or('\u{0305}')
}

pub fn build_kitty_placeholder_row(image_id: u32, row: usize, columns: u32) -> String {
    let row_diacritic = diacritic(row);
    let id_high_byte = (image_id >> 24) & 0xff;
    let id_diacritic = if id_high_byte > 0 {
        diacritic(id_high_byte as usize)
    } else {
        '\0'
    };
    let r = (image_id >> 16) & 0xff;
    let g = (image_id >> 8) & 0xff;
    let b = image_id & 0xff;

    let mut cells = String::new();
    for column in 0..columns {
        cells.push(KITTY_PLACEHOLDER);
        cells.push(row_diacritic);
        cells.push(diacritic(column as usize));
        if id_diacritic != '\0' {
            cells.push(id_diacritic);
        }
    }
    format!("\x1b[38;2;{r};{g};{b}m{cells}\x1b[39m")
}

pub fn delete_kitty_image(image_id: u32) -> String {
    let sequence = format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\");
    if get_capabilities().tmux_passthrough {
        wrap_tmux_passthrough(&sequence)
    } else {
        sequence
    }
}

pub fn delete_all_kitty_images() -> String {
    let sequence = "\x1b_Ga=d,d=A,q=2\x1b\\";
    if get_capabilities().tmux_passthrough {
        wrap_tmux_passthrough(sequence)
    } else {
        sequence.to_string()
    }
}

pub fn delete_all_kitty_placements() -> String {
    "\x1b_Ga=d,d=a,q=2\x1b\\".to_string()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Iterm2EncodeOptions {
    pub width: Option<Iterm2Dimension>,
    pub height: Option<Iterm2Dimension>,
    pub name: Option<&'static str>,
    pub preserve_aspect_ratio: bool,
    pub inline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Iterm2Dimension {
    Cells(u32),
    Auto,
}

impl Iterm2Dimension {
    fn render(self) -> String {
        match self {
            Iterm2Dimension::Cells(cells) => cells.to_string(),
            Iterm2Dimension::Auto => "auto".to_string(),
        }
    }
}

pub fn encode_iterm2(base64_data: &str, options: &Iterm2EncodeOptions) -> String {
    let mut params: Vec<String> = vec![
        format!("inline={}", if options.inline { 1 } else { 0 }),
        format!("size={}", decoded_base64_len(base64_data)),
    ];
    if let Some(width) = options.width {
        params.push(format!("width={}", width.render()));
    }
    if let Some(height) = options.height {
        params.push(format!("height={}", height.render()));
    }
    if let Some(name) = options.name.filter(|name| !name.is_empty()) {
        params.push(format!("name={}", encode_base64(name.as_bytes())));
    }
    if !options.preserve_aspect_ratio {
        params.push("preserveAspectRatio=0".to_string());
    }
    format!("\x1b]1337;File={}:{}\x07", params.join(";"), base64_data)
}

pub fn register_kitty_image_metadata(metadata: KittyImageMetadata) {
    let mut registry = mutex_lock(&KITTY_IMAGE_METADATA);
    registry.generation += 1;
    let generation = registry.generation;
    registry.entries.remove(&metadata.image_id);
    registry.order.retain(|id| *id != metadata.image_id);
    registry.entries.insert(
        metadata.image_id,
        RegisteredKittyImageMetadata {
            metadata,
            transmission_generation: generation,
        },
    );
    registry.order.push(metadata.image_id);
    if registry.order.len() > 1000
        && let Some(oldest) = registry.order.first().copied() {
            registry.entries.remove(&oldest);
            registry.order.remove(0);
        }
}

fn kitty_controls_before_semicolon(line: &str) -> Option<(usize, String)> {
    let start = line.find(KITTY_PREFIX)?;
    let controls_start = start + KITTY_PREFIX.len();
    let end = controls_start + line[controls_start..].find(';')?;
    Some((start, line[controls_start..end].to_string()))
}

fn parse_kitty_image_id(controls: &str) -> Option<u32> {
    for control in controls.split(',') {
        let (key, value) = match control.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        if key == "i" {
            return value.parse::<u32>().ok();
        }
    }
    None
}

fn registered_kitty_image_metadata(line: &str) -> Option<RegisteredKittyImageMetadata> {
    let (_, controls) = kitty_controls_before_semicolon(line)?;
    let image_id = parse_kitty_image_id(&controls)?;
    mutex_lock(&KITTY_IMAGE_METADATA)
        .entries
        .get(&image_id)
        .copied()
}

pub fn get_kitty_image_metadata(line: &str) -> Option<KittyImageMetadata> {
    registered_kitty_image_metadata(line).map(|registered| registered.metadata)
}

const KITTY_PLACEMENT_CONTROL_KEYS: [&str; 17] = [
    "i", "p", "x", "y", "w", "h", "X", "Y", "c", "r", "C", "U", "z", "P", "Q", "H", "V",
];

fn is_placement_control(control: &str) -> bool {
    let key = control.split_once('=').map(|(key, _)| key).unwrap_or(control);
    KITTY_PLACEMENT_CONTROL_KEYS.contains(&key)
}

fn is_transmission_continuation(controls: &str) -> bool {
    controls.split(',').any(|control| control == "m=1")
}

pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    let (match_index, controls) = kitty_controls_before_semicolon(line)?;
    let metadata = registered_kitty_image_metadata(line)?;

    let mut command_start = match_index;
    let mut command_controls = controls.clone();
    let transmission_end;
    loop {
        let terminator = line[command_start + KITTY_PREFIX.len()..].find("\x1b\\")?;
        let end = command_start + KITTY_PREFIX.len() + terminator + 2;
        if !is_transmission_continuation(&command_controls) {
            transmission_end = end;
            break;
        }
        command_start = end;
        if !line[command_start..].starts_with(KITTY_PREFIX) {
            return None;
        }
        let controls_start = command_start + KITTY_PREFIX.len();
        let controls_end = controls_start + line[controls_start..].find(';')?;
        command_controls = line[controls_start..controls_end].to_string();
    }

    let filtered: Vec<&str> = controls.split(',').filter(|c| is_placement_control(c)).collect();
    let sequence = format!("\x1b_Ga=p,q=2,{}\x1b\\", filtered.join(","));
    let image_id = metadata.metadata.image_id;
    let replacement_line = format!(
        "{}{}{}",
        &line[..match_index],
        sequence,
        &line[transmission_end..]
    );
    Some(KittyImagePlacement {
        image_id,
        transmission_generation: metadata.transmission_generation,
        transmission_bytes: (transmission_end - match_index) as u64,
        estimated_decoded_bytes: u64::from(metadata.metadata.width_px)
            * u64::from(metadata.metadata.height_px)
            * 4,
        sequence,
        replacement_line,
    })
}

pub fn crop_kitty_image_line(line: &str, hidden_rows: usize, visible_rows: usize) -> String {
    let Some(metadata) = get_kitty_image_metadata(line) else {
        return line.to_string();
    };
    let Some((match_index, controls)) = kitty_controls_before_semicolon(line) else {
        return line.to_string();
    };
    if hidden_rows >= metadata.rows as usize || visible_rows == 0 {
        return line.to_string();
    }
    let cropped_rows = visible_rows.min(metadata.rows as usize - hidden_rows);
    if hidden_rows == 0 && cropped_rows == metadata.rows as usize {
        return line.to_string();
    }
    let source_y = (f64::from(metadata.height_px) * hidden_rows as f64 / f64::from(metadata.rows))
        .floor();
    let source_end = (f64::from(metadata.height_px) * (hidden_rows + cropped_rows) as f64
        / f64::from(metadata.rows))
    .ceil();
    let source_height = 1f64.max(f64::from(metadata.height_px).min(source_end) - source_y);
    let mut new_controls: Vec<String> = controls
        .split(',')
        .filter(|control| {
            !(control.starts_with("y=") || control.starts_with("h=") || control.starts_with("r="))
        })
        .map(str::to_string)
        .collect();
    new_controls.push(format!("y={}", source_y as u32));
    new_controls.push(format!("h={}", source_height as u32));
    new_controls.push(format!("r={cropped_rows}"));

    let params_end = match_index + KITTY_PREFIX.len() + controls.len() + 1;
    format!(
        "{}\x1b_G{};{}",
        &line[..match_index],
        new_controls.join(","),
        &line[params_end..]
    )
}

pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: f64,
    max_height_cells: Option<f64>,
    cell_dimensions: CellDimensions,
) -> ImageCellSize {
    let max_width = 1f64.max(max_width_cells.floor());
    let max_height = max_height_cells.map(|height| 1f64.max(height.floor()));
    let image_width = f64::from(image_dimensions.width_px.max(1));
    let image_height = f64::from(image_dimensions.height_px.max(1));
    let cell_width = f64::from(cell_dimensions.width_px.max(1));
    let cell_height = f64::from(cell_dimensions.height_px.max(1));

    let width_scale = (max_width * cell_width) / image_width;
    let height_scale = match max_height {
        Some(max_height) => (max_height * cell_height) / image_height,
        None => width_scale,
    };
    let scale = width_scale.min(height_scale);

    let scaled_width_px = image_width * scale;
    let scaled_height_px = image_height * scale;
    let columns = (scaled_width_px / cell_width).ceil();
    let rows = (scaled_height_px / cell_height).ceil();

    ImageCellSize {
        columns: 1f64.max(max_width.min(columns)) as u32,
        rows: 1f64.max(match max_height {
            Some(max_height) => max_height.min(rows),
            None => rows,
        }) as u32,
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn decode_base64(input: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    out
}

pub fn decoded_base64_len(input: &str) -> usize {
    decode_base64(input).len()
}

pub fn encode_base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map(u32::from);
        let b2 = chunk.get(2).copied().map(u32::from);
        let triple = (b0 << 16) | (b1.unwrap_or(0) << 8) | b2.unwrap_or(0);
        out.push(BASE64_ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if b1.is_some() {
            BASE64_ALPHABET[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if b2.is_some() {
            BASE64_ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn read_u32_be(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u16_be(buffer: &[u8], offset: usize) -> Option<u16> {
    let bytes = buffer.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u16_le(buffer: &[u8], offset: usize) -> Option<u16> {
    let bytes = buffer.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32_le(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data);
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0] != 0x89 || buffer[1] != 0x50 || buffer[2] != 0x4e || buffer[3] != 0x47 {
        return None;
    }
    Some(ImageDimensions {
        width_px: read_u32_be(&buffer, 16)?,
        height_px: read_u32_be(&buffer, 20)?,
    })
}

pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data);
    if buffer.len() < 2 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    let mut offset = 2usize;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            return Some(ImageDimensions {
                width_px: u32::from(read_u16_be(&buffer, offset + 7)?),
                height_px: u32::from(read_u16_be(&buffer, offset + 5)?),
            });
        }
        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = read_u16_be(&buffer, offset + 2)?;
        if length < 2 {
            return None;
        }
        offset += 2 + usize::from(length);
    }
    None
}

pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data);
    if buffer.len() < 10 {
        return None;
    }
    let signature = String::from_utf8_lossy(&buffer[0..6]);
    if signature != "GIF87a" && signature != "GIF89a" {
        return None;
    }
    Some(ImageDimensions {
        width_px: u32::from(read_u16_le(&buffer, 6)?),
        height_px: u32::from(read_u16_le(&buffer, 8)?),
    })
}

pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data);
    if buffer.len() < 30 {
        return None;
    }
    let riff = String::from_utf8_lossy(&buffer[0..4]);
    let webp = String::from_utf8_lossy(&buffer[8..12]);
    if riff != "RIFF" || webp != "WEBP" {
        return None;
    }
    let chunk = String::from_utf8_lossy(&buffer[12..16]);
    if chunk == "VP8 " {
        if buffer.len() < 30 {
            return None;
        }
        return Some(ImageDimensions {
            width_px: u32::from(read_u16_le(&buffer, 26)? & 0x3fff),
            height_px: u32::from(read_u16_le(&buffer, 28)? & 0x3fff),
        });
    }
    if chunk == "VP8L" {
        if buffer.len() < 25 {
            return None;
        }
        let bits = read_u32_le(&buffer, 21)?;
        return Some(ImageDimensions {
            width_px: (bits & 0x3fff) + 1,
            height_px: ((bits >> 14) & 0x3fff) + 1,
        });
    }
    if chunk == "VP8X" {
        if buffer.len() < 30 {
            return None;
        }
        let width = u32::from(buffer[24]) | (u32::from(buffer[25]) << 8) | (u32::from(buffer[26]) << 16);
        let height = u32::from(buffer[27]) | (u32::from(buffer[28]) << 8) | (u32::from(buffer[29]) << 16);
        return Some(ImageDimensions {
            width_px: width + 1,
            height_px: height + 1,
        });
    }
    None
}

pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: &ImageRenderOptions,
) -> Option<ImageRenderResult> {
    let caps = get_capabilities();
    let images = caps.images?;

    let max_width = options.max_width_cells.unwrap_or(80);
    let size = calculate_image_cell_size(
        image_dimensions,
        f64::from(max_width),
        options.max_height_cells.map(f64::from),
        get_cell_dimensions(),
    );

    if images == ImageProtocol::Kitty && caps.kitty_unicode_placeholders {
        let image_id = options.image_id.unwrap_or_else(allocate_image_id);
        let columns = size.columns.min(KITTY_PLACEHOLDER_MAX as u32);
        let rows = size.rows.min(KITTY_PLACEHOLDER_MAX as u32);
        let sequence = encode_kitty(
            base64_data,
            &KittyEncodeOptions {
                columns: Some(columns),
                rows: Some(rows),
                image_id: Some(image_id),
                move_cursor: true,
                virtual_placement: true,
            },
        );
        let mut lines: Vec<String> = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let placeholder_row = build_kitty_placeholder_row(image_id, row as usize, columns);
            lines.push(if row == 0 {
                format!("{sequence}{placeholder_row}")
            } else {
                placeholder_row
            });
        }
        return Some(ImageRenderResult {
            sequence,
            columns,
            rows,
            image_id: Some(image_id),
            lines: Some(lines),
        });
    }

    if images == ImageProtocol::Kitty {
        if let Some(image_id) = options.image_id {
            register_kitty_image_metadata(KittyImageMetadata {
                image_id,
                columns: size.columns,
                rows: size.rows,
                width_px: image_dimensions.width_px,
                height_px: image_dimensions.height_px,
            });
        }
        let sequence = encode_kitty(
            base64_data,
            &KittyEncodeOptions {
                columns: Some(size.columns),
                rows: Some(size.rows),
                image_id: options.image_id,
                move_cursor: options.move_cursor,
                virtual_placement: false,
            },
        );
        return Some(ImageRenderResult {
            sequence,
            columns: size.columns,
            rows: size.rows,
            image_id: options.image_id,
            lines: None,
        });
    }

    if images == ImageProtocol::Iterm2 {
        let sequence = encode_iterm2(
            base64_data,
            &Iterm2EncodeOptions {
                width: Some(Iterm2Dimension::Cells(size.columns)),
                height: Some(Iterm2Dimension::Auto),
                name: None,
                preserve_aspect_ratio: options.preserve_aspect_ratio,
                inline: true,
            },
        );
        return Some(ImageRenderResult {
            sequence,
            columns: size.columns,
            rows: size.rows,
            image_id: None,
            lines: None,
        });
    }

    None
}

pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

pub fn path_to_file_url(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for character in path.chars() {
        match character {
            '%' => escaped.push_str("%25"),
            '\\' => escaped.push_str("%5C"),
            '\n' => escaped.push_str("%0A"),
            '\r' => escaped.push_str("%0D"),
            '\t' => escaped.push_str("%09"),
            _ => escaped.push(character),
        }
    }
    let mut out = String::from("file://");
    for byte in escaped.bytes() {
        if byte <= 0x20
            || byte >= 0x7f
            || matches!(byte, b'"' | b'#' | b'<' | b'>' | b'?' | b'`' | b'{' | b'}')
        {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(byte as char);
        }
    }
    out
}

fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with('\\')
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'/' | b'\\'))
}

pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(filename) = filename.filter(|name| !name.is_empty()) {
        let sanitized = sanitize_terminal_label(filename);
        let display = shorten_image_path(&sanitized);
        if get_capabilities().hyperlinks && is_absolute_path(&sanitized) {
            parts.push(hyperlink(&display, &path_to_file_url(&sanitized)));
        } else {
            parts.push(display);
        }
    }
    parts.push(format!("[{}]", sanitize_terminal_label(mime_type)));
    if let Some(dimensions) = dimensions {
        parts.push(format!("{}x{}", dimensions.width_px, dimensions.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

#[cfg(test)]
#[path = "terminal_image_tests.rs"]
mod tests;
