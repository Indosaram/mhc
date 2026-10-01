//! Port of senpi packages/ai/src/auth/oauth/kimi-identity.ts.
//!
//! The agent dir and its env override carry the maho brand (D-M7): `MAHO_CODING_AGENT_DIR` and
//! `~/.maho/agent`, matching `utils/cursor_context_limit.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

const KIMI_CLIENT_PLATFORM: &str = "kimi_cli";
const KIMI_CLIENT_VERSION: &str = "1.0";
const DEVICE_ID_FILENAME: &str = "kimi-device-id";

thread_local! {
    static ENV_OVERRIDES: RefCell<Vec<HashMap<String, Option<String>>>> = const { RefCell::new(Vec::new()) };
}

pub fn env_value(name: &str) -> Option<String> {
    let overridden = ENV_OVERRIDES.with(|stack| {
        stack.borrow().iter().rev().find_map(|layer| layer.get(name).cloned())
    });
    match overridden {
        Some(value) => value,
        None => std::env::var(name).ok(),
    }
}

pub fn with_env_overrides<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            ENV_OVERRIDES.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }
    let layer: HashMap<String, Option<String>> =
        vars.iter().map(|(key, value)| ((*key).to_string(), value.map(str::to_string))).collect();
    ENV_OVERRIDES.with(|stack| stack.borrow_mut().push(layer));
    let _pop = Pop;
    f()
}

fn sanitize_header_value(value: &str, fallback: &str) -> String {
    let sanitized: String = value.chars().filter(|ch| ('\u{20}'..='\u{7e}').contains(ch)).collect();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() { fallback.to_string() } else { sanitized.to_string() }
}

fn agent_dir() -> String {
    let configured = env_value("MAHO_CODING_AGENT_DIR").or_else(|| env_value("CODING_AGENT_DIR"));
    if let Some(configured) = configured {
        return configured.trim_end_matches('/').to_string();
    }
    let home = env_value("HOME").unwrap_or_else(|| ".".to_string());
    format!("{}/.maho/agent", home.trim_end_matches('/'))
}

fn platform() -> String {
    match std::env::consts::OS {
        "macos" => "macOS".to_string(),
        "windows" => "Windows".to_string(),
        "linux" => "Linux".to_string(),
        other => other.to_string(),
    }
}

fn release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        "x86" => "ia32".to_string(),
        other => other.to_string(),
    }
}

fn hostname() -> String {
    if let Some(name) = env_value("HOSTNAME")
        && !name.trim().is_empty() {
            return name;
        }
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|value| value.trim().to_string()).unwrap_or_default()
}

fn device_model() -> String {
    let current = platform();
    [current, release(), arch()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ").trim().to_string()
}

fn read_or_mint_device_id() -> String {
    let directory = agent_dir();
    let path = std::path::Path::new(&directory).join(DEVICE_ID_FILENAME);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return existing.to_string();
        }
    }

    let minted = uuid::Uuid::new_v4().to_string().replace('-', "");
    if std::fs::create_dir_all(&directory).is_ok()
        && let Ok(mut file) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&path) {
            use std::io::Write as _;
            let _ = file.write_all(format!("{minted}\n").as_bytes());
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
    minted
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

static CACHED_DEVICE_ID: Mutex<Option<String>> = Mutex::new(None);

fn device_id() -> String {
    let mut cached = CACHED_DEVICE_ID.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(existing) = cached.clone() {
        return existing;
    }
    let minted = read_or_mint_device_id();
    *cached = Some(minted.clone());
    minted
}

pub fn reset_kimi_device_id_for_tests() {
    *CACHED_DEVICE_ID.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

pub fn kimi_code_identity_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), format!("KimiCLI/{KIMI_CLIENT_VERSION}")),
        ("X-Msh-Platform".into(), KIMI_CLIENT_PLATFORM.to_string()),
        ("X-Msh-Version".into(), KIMI_CLIENT_VERSION.to_string()),
        ("X-Msh-Device-Name".into(), sanitize_header_value(&hostname(), "unknown")),
        ("X-Msh-Device-Model".into(), sanitize_header_value(&device_model(), "unknown")),
        ("X-Msh-Os-Version".into(), sanitize_header_value(&release(), "unknown")),
        ("X-Msh-Device-Id".into(), sanitize_header_value(&device_id(), "unknown")),
    ]
}

pub fn kimi_code_identity_header_map() -> HashMap<String, String> {
    kimi_code_identity_headers().into_iter().collect()
}
