//! `runtimeBuildId`: which runtime a process actually loaded, as one content digest (senpi
//! `rpc/runtime-build-id.ts`). The id is `sha256:<64 hex>` over a canonical manifest built from contents
//! only: flavour, platform and architecture, the engine build text, the digests of the runtime files sorted
//! by relative path, one digest per launch-profile extension, and the launch-profile flags that change what a
//! host loads. No absolute path and no mutable state enters the manifest.
//!
//! A host computes its id ONCE, at startup; a binary replaced later at the same path leaves the running
//! host's id unchanged while a client started from the new binary computes a different one.
pub use crate::host_handover_request::RUNTIME_IDENTITY_HANDOVER_CAPABILITY;
use crate::host_protocol_info::{RpcLaunchProfileCore, SessionRuntimeKind};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const BUILTIN_PATH_PREFIX: &str = "builtin:";
const EXCLUDED_NAMES: [&str; 3] = [
    "node_modules",
    "runtime-manifest.json",
    "runtime-snapshot.json",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeFlavour {
    Compiled,
    Packaged,
    Dev,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSource {
    pub flavour: RuntimeFlavour,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeBuildIdInput {
    pub extensions: Vec<String>,
    pub multi_session: bool,
    pub session_runtime: Option<SessionRuntimeKind>,
    /// Defaults to the runtime this process was loaded from.
    pub source: Option<RuntimeSource>,
    pub platform: Option<String>,
    pub arch: Option<String>,
    pub engine: Option<String>,
}

impl RuntimeBuildIdInput {
    pub fn from_profile(profile: &RpcLaunchProfileCore) -> Self {
        Self {
            extensions: profile.extensions.clone(),
            multi_session: profile.multi_session,
            session_runtime: Some(profile.session_runtime),
            ..Self::default()
        }
    }
}

/// A well-formed id, or `None`: anything else proves nothing.
pub fn parse_runtime_build_id(value: &serde_json::Value) -> Option<&str> {
    let text = value.as_str()?;
    let hex = text.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(text)
}

/// Node's `process.platform` / `process.arch` names, which the manifest carries.
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

#[derive(Serialize)]
struct ManifestProfile {
    multi_session: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_runtime: Option<SessionRuntimeKind>,
}

#[derive(Serialize)]
struct Manifest {
    schema: u32,
    flavour: RuntimeFlavour,
    platform: String,
    arch: String,
    engine: String,
    runtime: String,
    plugins: Vec<String>,
    profile: ManifestProfile,
}

pub fn compute_runtime_build_id(input: &RuntimeBuildIdInput) -> std::io::Result<String> {
    let source = match &input.source {
        Some(source) => source.clone(),
        None => loaded_runtime_source()?,
    };
    let mut plugins = input
        .extensions
        .iter()
        .map(|path| extension_digest(path))
        .collect::<std::io::Result<Vec<_>>>()?;
    plugins.sort();
    let manifest = Manifest {
        schema: 1,
        flavour: source.flavour,
        platform: input
            .platform
            .clone()
            .unwrap_or_else(|| node_platform().into()),
        arch: input.arch.clone().unwrap_or_else(|| node_arch().into()),
        engine: input.engine.clone().unwrap_or_else(|| {
            maho_core::engine_build_identity::engine_build_identity()
                .text
                .clone()
        }),
        runtime: tree_digest(&source.root)?,
        plugins,
        profile: ManifestProfile {
            multi_session: input.multi_session,
            session_runtime: input.session_runtime,
        },
    };
    let text = serde_json::to_string(&manifest).map_err(std::io::Error::other)?;
    Ok(format!("sha256:{}", sha256(text.as_bytes())))
}

/// Where the code of THIS process lives: a compiled binary is its own executable.
pub fn loaded_runtime_source() -> std::io::Result<RuntimeSource> {
    Ok(RuntimeSource {
        flavour: RuntimeFlavour::Compiled,
        root: std::env::current_exe()?,
    })
}

/// A `builtin:<name>` extension is code inside the runtime, already in its digest; only its name counts.
fn extension_digest(path: &str) -> std::io::Result<String> {
    let name = Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    if name.starts_with(BUILTIN_PATH_PREFIX) {
        return Ok(sha256(name.as_bytes()));
    }
    tree_digest(Path::new(path))
}

fn tree_digest(root: &Path) -> std::io::Result<String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if std::fs::metadata(root)?.is_file() {
        files.push((String::new(), root.to_path_buf()));
    } else {
        collect(
            &std::fs::canonicalize(root)?,
            "",
            &mut files,
            &mut HashSet::new(),
        )?;
    }
    let mut digests = files
        .into_iter()
        .map(|(path, file)| Ok((path, file_sha256(&file)?)))
        .collect::<std::io::Result<Vec<(String, String)>>>()?;
    digests.sort_by(|(left, _), (right, _)| left.cmp(right));
    let text = serde_json::to_string(&digests).map_err(std::io::Error::other)?;
    Ok(sha256(text.as_bytes()))
}

fn excluded_file(name: &str) -> bool {
    name.ends_with(".map")
        || [".d.ts", ".d.cts", ".d.mts"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// A real directory is digested once, under the first (sorted) name that reaches it, so a symlinked alias
/// never adds a second copy of the same files.
fn collect(
    dir: &Path,
    prefix: &str,
    files: &mut Vec<(String, PathBuf)>,
    seen: &mut HashSet<PathBuf>,
) -> std::io::Result<()> {
    if !seen.insert(dir.to_path_buf()) {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || EXCLUDED_NAMES.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // `metadata` follows a symlink; a dangling one is skipped.
        let Ok(kind) = std::fs::metadata(&path) else {
            continue;
        };
        if kind.is_dir() {
            collect(&std::fs::canonicalize(&path)?, &relative, files, seen)?;
        } else if kind.is_file() && !excluded_file(&name) {
            files.push((relative, path));
        }
    }
    Ok(())
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// A file's digest, streamed: a compiled runtime is its own (large) executable, never held in memory whole.
fn file_sha256(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(root: &Path, extensions: Vec<String>) -> RuntimeBuildIdInput {
        RuntimeBuildIdInput {
            extensions,
            multi_session: true,
            session_runtime: Some(SessionRuntimeKind::Worker),
            source: Some(RuntimeSource {
                flavour: RuntimeFlavour::Packaged,
                root: root.to_path_buf(),
            }),
            platform: Some("linux".into()),
            arch: Some("x64".into()),
            engine: Some("2026.10.1".into()),
        }
    }

    #[test]
    fn the_id_depends_on_contents_not_location_or_mutable_state() {
        let one = tempfile::tempdir().expect("tempdir");
        let two = tempfile::tempdir().expect("tempdir");
        for dir in [one.path(), two.path()] {
            std::fs::create_dir_all(dir.join("lib")).expect("mkdir");
            std::fs::write(dir.join("lib/a.js"), "a").expect("write");
        }
        std::fs::write(two.path().join("lib/a.js.map"), "map").expect("write");
        std::fs::write(two.path().join("runtime-manifest.json"), "{}").expect("write");
        std::fs::write(two.path().join(".cache"), "x").expect("write");
        let first = compute_runtime_build_id(&input(one.path(), vec![])).expect("id");
        assert_eq!(
            first,
            compute_runtime_build_id(&input(two.path(), vec![])).expect("id")
        );
        assert!(parse_runtime_build_id(&serde_json::json!(first)).is_some());
        std::fs::write(two.path().join("lib/a.js"), "b").expect("write");
        assert_ne!(
            first,
            compute_runtime_build_id(&input(two.path(), vec![])).expect("id")
        );
    }

    #[test]
    fn manifest_bytes_match_the_upstream_shape() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("a.js"), "a").expect("write");
        let id = compute_runtime_build_id(&input(root.path(), vec!["/x/builtin:tools".into()]))
            .expect("id");
        let tree = sha256(format!(r#"[["a.js","{}"]]"#, sha256(b"a")).as_bytes());
        let manifest = format!(
            r#"{{"schema":1,"flavour":"packaged","platform":"linux","arch":"x64","engine":"2026.10.1","runtime":"{tree}","plugins":["{}"],"profile":{{"multi_session":true,"session_runtime":"worker"}}}}"#,
            sha256(b"builtin:tools")
        );
        assert_eq!(id, format!("sha256:{}", sha256(manifest.as_bytes())));
    }

    #[test]
    fn only_well_formed_ids_parse() {
        assert!(
            parse_runtime_build_id(&serde_json::json!(format!("sha256:{}", "a".repeat(64))))
                .is_some()
        );
        assert!(
            parse_runtime_build_id(&serde_json::json!(format!("sha256:{}", "A".repeat(64))))
                .is_none()
        );
        assert!(parse_runtime_build_id(&serde_json::json!("sha1:abc")).is_none());
        assert!(parse_runtime_build_id(&serde_json::json!(1)).is_none());
    }
}
