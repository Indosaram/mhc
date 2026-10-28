//! Shared fixtures for the native update integration tests.

#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, Command};

pub const NEW_VERSION: &str = "9999.1.1";
pub const NEWER_VERSION: &str = "9999.2.1";

const RUN_TIMEOUT: Duration = Duration::from_secs(120);
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const SIBLINGS: [&str; 4] = ["mhc-lsp", "senpi-desktop-engine", "mhc-git-bash", "mhc-ast-grep"];

pub fn real_version() -> String {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(read_real_version).clone()
}

fn read_real_version() -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .arg("--version")
        .output()
        .expect("run mhc --version");
    if !output.status.success() {
        panic!("mhc --version failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8(output.stdout).expect("mhc --version is utf-8");
    let trimmed = text.trim();
    trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed).to_owned()
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub agent: PathBuf,
}

pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("fixture directory");
    let home = dir.path().join("home");
    let agent = dir.path().join("agent");
    let root = dir.path().join("root");
    let native = agent.join("omo-senpi").join("omo-native");
    std::fs::create_dir_all(&native).expect("agent state directory");
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::create_dir_all(&root).expect("install root");
    std::fs::write(native.join("onboarding-completed"), "{}\n").expect("onboarding marker");
    let settings = json!({"theme": "dark", "customUserSetting": "keep"});
    std::fs::write(agent.join("settings.json"), format!("{settings}\n")).expect("settings.json");
    std::fs::write(home.join(".sentinel"), "sentinel").expect("home sentinel");
    Fixture { dir, root, home, agent }
}

pub enum Payload {
    RealMhc,
    Probe { version: String, fail_when_installed: bool },
}

pub fn build_package(out_parent: &Path, payload: Payload) -> PathBuf {
    let target = maho_cli::native_update::release::target().expect("native update target");
    let package = out_parent.join(format!("mhc-{target}"));
    std::fs::create_dir_all(&package).expect("package directory");
    let mhc_bin = match &payload {
        Payload::RealMhc => env!("CARGO_BIN_EXE_mhc"),
        Payload::Probe { .. } => env!("CARGO_BIN_EXE_mhc-update-probe"),
    };
    let mut executables = vec![
        copy_executable(&package, "mhc", mhc_bin),
        copy_executable(&package, "mhc-launcher", env!("CARGO_BIN_EXE_mhc-launcher")),
    ];
    for name in SIBLINGS {
        executables.push(copy_executable(
            &package,
            name,
            env!("CARGO_BIN_EXE_mhc-update-probe"),
        ));
    }
    let license = write_data(&package, "licenses/LICENSE", b"licensed\n");
    let skill = write_data(&package, "skills/s/SKILL.md", b"# s\n");
    let mut manifest = json!({
        "schemaVersion": 1,
        "executables": executables,
        "licenses": [license],
        "skills": { "files": [skill] },
    });
    if let Payload::Probe { version, fail_when_installed } = payload {
        manifest["probeVersion"] = Value::String(version);
        manifest["probeFailInstalled"] = Value::Bool(fail_when_installed);
    }
    let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize package manifest");
    std::fs::write(package.join("manifest.json"), bytes).expect("write package manifest");
    package
}

pub fn archive(package_dir: &Path, out_dir: &Path) -> (PathBuf, String) {
    let target = maho_cli::native_update::release::target().expect("native update target");
    let asset = maho_cli::native_update::release::asset_name_for_target(target)
        .expect("release asset name");
    std::fs::create_dir_all(out_dir).expect("archive output directory");
    let path = out_dir.join(&asset);
    let top = format!("mhc-{target}");
    let written = if asset.ends_with(".zip") {
        write_zip(package_dir, &path, &top)
    } else {
        write_tar_gz(package_dir, &path, &top)
    };
    if let Err(error) = written {
        panic!("write {}: {error}", path.display());
    }
    let digest = sha256_file(&path);
    (path, digest)
}

pub fn install_versioned(fx: &Fixture, version: &str, package_dir: &Path, previous: Option<&str>) {
    let dest = fx.root.join("versions").join(version);
    copy_tree(package_dir, &dest);
    let launcher_name = exe_name("mhc-launcher");
    let installed = launcher(fx);
    if let Err(error) = std::fs::copy(dest.join(&launcher_name), &installed) {
        panic!("install launcher: {error}");
    }
    set_mode(&installed, 0o755);
    let mut bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "current": version,
        "previous": previous,
    }))
    .expect("serialize current.json");
    bytes.push(b'\n');
    std::fs::write(fx.root.join("current.json"), bytes).expect("write current.json");
}

pub struct ReleaseServer {
    pub url: String,
    pub requests: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for ReleaseServer {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// How the served release publishes its minisign signature.
pub enum Signing {
    /// Signed with the fixture test key (the key the test `mhc` binary embeds).
    Valid,
    /// Signed with the fixture test key, but over these bytes instead of the served archive.
    ValidFor(Vec<u8>),
    /// No `.minisig` asset.
    Missing,
    /// Signed with a freshly generated key the binary does not trust.
    OtherKey,
}

pub const TEST_PUBKEY_FILE: &str = "tests/fixtures/release-test-key/test.pub";

fn sign_with_test_key(archive: &[u8]) -> String {
    let key = include_str!("../fixtures/release-test-key/test.key");
    let secret = minisign::SecretKeyBox::from_string(key)
        .and_then(|boxed| boxed.into_secret_key(Some(String::new())))
        .expect("fixture test key");
    minisign::sign(None, &secret, std::io::Cursor::new(archive), Some("mhc test"), None)
        .expect("sign with fixture key")
        .into_string()
}

fn sign_with_other_key(archive: &[u8]) -> String {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().expect("generate key pair");
    minisign::sign(None, &pair.sk, std::io::Cursor::new(archive), Some("mhc other"), None)
        .expect("sign with other key")
        .into_string()
}

pub async fn serve_release(version: &str, archive: &Path, checksum_line: String) -> ReleaseServer {
    serve_release_signed(version, archive, checksum_line, Signing::Valid).await
}

pub async fn serve_release_signed(
    version: &str,
    archive: &Path,
    checksum_line: String,
    signing: Signing,
) -> ReleaseServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind release server");
    let port = listener.local_addr().expect("release server address").port();
    let asset_name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .expect("archive file name")
        .to_owned();
    let checksum_name = format!("{asset_name}.sha256");
    let signature_name = format!("{asset_name}.minisig");
    let signature_path = format!("/assets/{signature_name}");
    let archive_bytes = tokio::fs::read(archive).await.expect("read release archive");
    let signature = match signing {
        Signing::Valid => Some(sign_with_test_key(&archive_bytes)),
        Signing::ValidFor(bytes) => Some(sign_with_test_key(&bytes)),
        Signing::OtherKey => Some(sign_with_other_key(&archive_bytes)),
        Signing::Missing => None,
    };
    let asset_path = format!("/assets/{asset_name}");
    let checksum_path = format!("/assets/{checksum_name}");
    let asset_url = format!("http://127.0.0.1:{port}{asset_path}");
    let checksum_url = format!("http://127.0.0.1:{port}{checksum_path}");
    let mut assets = vec![
        json!({"name": asset_name, "browser_download_url": asset_url}),
        json!({"name": checksum_name, "browser_download_url": checksum_url}),
    ];
    if signature.is_some() {
        let signature_url = format!("http://127.0.0.1:{port}{signature_path}");
        assets.push(json!({"name": signature_name, "browser_download_url": signature_url}));
    }
    let metadata = serde_json::to_vec(&json!({
        "tag_name": format!("v{version}"),
        "draft": false,
        "prerelease": false,
        "assets": assets,
    }))
    .expect("release metadata");
    let requests = Arc::new(AtomicUsize::new(0));
    let served = ServedRelease {
        latest_path: "/repos/Indosaram/mhc/releases/latest".to_owned(),
        asset_path,
        checksum_path,
        metadata,
        archive: archive_bytes,
        checksum: checksum_line.into_bytes(),
        signature_path,
        signature: signature.unwrap_or_default().into_bytes(),
        requests: Arc::clone(&requests),
    };
    let url = format!("http://127.0.0.1:{port}/repos/Indosaram/mhc/releases/latest");
    let _task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let _ = serve_one(&mut socket, &served).await;
        }
    });
    ReleaseServer { url, requests, _task }
}

pub fn mhc(fx: &Fixture, exe: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .current_dir(&fx.home)
        .env("HOME", &fx.home)
        .env("MAHO_CODING_AGENT_DIR", &fx.agent)
        .env_remove("__PI_INTERNAL_SPAWN")
        .env_remove("MHC_UPDATE_RELEASE_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        command.env("USERPROFILE", &fx.home);
    }
    command
}

pub async fn run(mut cmd: Command) -> std::process::Output {
    let mut child = cmd.spawn().expect("spawn mhc");
    let mut stdout_pipe = child.stdout.take().expect("mhc stdout");
    let mut stderr_pipe = child.stderr.take().expect("mhc stderr");
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let result = stdout_pipe.read_to_end(&mut buf).await;
        (result, buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let result = stderr_pipe.read_to_end(&mut buf).await;
        (result, buf)
    });
    let status = match tokio::time::timeout(RUN_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => panic!("wait for mhc: {error}"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            panic!("mhc did not exit within 120s");
        }
    };
    let (stdout_result, stdout) = stdout_task.await.expect("mhc stdout task");
    let (stderr_result, stderr) = stderr_task.await.expect("mhc stderr task");
    stdout_result.expect("read mhc stdout");
    stderr_result.expect("read mhc stderr");
    std::process::Output { status, stdout, stderr }
}

pub fn launcher(fx: &Fixture) -> PathBuf {
    fx.root.join(exe_name("mhc"))
}

pub fn pointer(fx: &Fixture) -> (String, Option<String>) {
    let bytes = std::fs::read(fx.root.join("current.json")).expect("read current.json");
    let value: Value = serde_json::from_slice(&bytes).expect("parse current.json");
    let current = value.get("current").and_then(Value::as_str).expect("current version").to_owned();
    let previous = value.get("previous").and_then(Value::as_str).map(str::to_owned);
    (current, previous)
}

pub fn versions(fx: &Fixture) -> Vec<String> {
    let directory = fx.root.join("versions");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("list {}: {error}", directory.display()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("read version entry: {error}"));
        let kind = entry.file_type().unwrap_or_else(|error| panic!("version entry type: {error}"));
        let file_name = entry.file_name();
        if kind.is_dir()
            && let Some(name) = file_name.to_str()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names
}

pub fn state_untouched(fx: &Fixture) -> bool {
    let Ok(sentinel) = std::fs::read(fx.home.join(".sentinel")) else { return false };
    if sentinel != b"sentinel" {
        return false;
    }
    let Ok(bytes) = std::fs::read(fx.agent.join("settings.json")) else { return false };
    let Ok(settings) = serde_json::from_slice::<Value>(&bytes) else { return false };
    settings.get("theme").and_then(Value::as_str) == Some("dark")
        && settings.get("customUserSetting").and_then(Value::as_str) == Some("keep")
}

pub fn probe_marker(fx: &Fixture) -> PathBuf {
    fx.dir.path().join("probe-marker")
}

pub struct Session {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    pub pid: u32,
    stderr: tokio::task::JoinHandle<()>,
}

pub async fn spawn_session(fx: &Fixture) -> Session {
    let mut command = mhc(fx, &launcher(fx));
    command.stdin(Stdio::piped()).args([
        "--mode",
        "rpc",
        "--offline",
        "--no-session",
        "--model",
        "openai/gpt-4o",
    ]);
    let mut child = command.spawn().expect("spawn mhc session");
    let mut stdin = child.stdin.take().expect("session stdin");
    let stdout = child.stdout.take().expect("session stdout");
    let mut stderr = child.stderr.take().expect("session stderr");
    let pid = child.id().expect("session pid");
    // Drain stderr so a long-lived session cannot block on a full pipe.
    let stderr_task = tokio::spawn(async move {
        let mut sink = [0u8; 8192];
        loop {
            match stderr.read(&mut sink).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    stdin
        .write_all(b"{\"id\":\"ready\",\"type\":\"get_messages\"}\n")
        .await
        .expect("session ready request");
    stdin.flush().await.expect("flush session ready request");
    let mut stdout = tokio::io::BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(SESSION_TIMEOUT, wait_for_id(&mut stdout, "ready")).await;
    if !matches!(ready, Ok(true)) {
        let _ = child.kill().await;
        let _ = child.wait().await;
        stderr_task.abort();
        panic!("session did not answer id \"ready\" within 60s");
    }
    Session { child, stdin, stdout, pid, stderr: stderr_task }
}

impl Session {
    pub async fn alive_roundtrip(&mut self, id: &str) -> bool {
        let Ok(body) = serde_json::to_string(&json!({"id": id, "type": "get_messages"})) else {
            return false;
        };
        let line = format!("{body}\n");
        if self.stdin.write_all(line.as_bytes()).await.is_err() || self.stdin.flush().await.is_err()
        {
            return false;
        }
        matches!(
            tokio::time::timeout(SESSION_TIMEOUT, wait_for_id(&mut self.stdout, id)).await,
            Ok(true)
        )
    }

    pub fn executable(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            let pid = self.pid;
            std::fs::read_link(format!("/proc/{pid}/exe")).ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = self.pid;
            None
        }
    }

    pub async fn close(mut self) {
        drop(self.stdin);
        match tokio::time::timeout(SESSION_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_)) => {}
            _ => {
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
            }
        }
        self.stderr.abort();
    }
}

fn exe_name(name: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    format!("{name}{suffix}")
}

fn package_join(package: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(package.to_path_buf(), |path, part| path.join(part))
}

fn copy_executable(package: &Path, name: &str, source: &str) -> Value {
    let rel = exe_name(name);
    let destination = package_join(package, &rel);
    if let Err(error) = std::fs::copy(source, &destination) {
        panic!("copy {source} to {rel}: {error}");
    }
    set_mode(&destination, 0o755);
    file_record(&rel, &destination, "755")
}

fn write_data(package: &Path, rel: &str, bytes: &[u8]) -> Value {
    let destination = package_join(package, rel);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|error| panic!("create {}: {error}", parent.display()));
    }
    if let Err(error) = std::fs::write(&destination, bytes) {
        panic!("write {rel}: {error}");
    }
    set_mode(&destination, 0o644);
    file_record(rel, &destination, "644")
}

fn file_record(rel: &str, path: &Path, mode: &str) -> Value {
    json!({
        "path": rel,
        "sha256": sha256_file(path),
        "size": std::fs::metadata(path).expect("package file size").len(),
        "mode": mode,
    })
}

fn sha256_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    format!("{:x}", hasher.finalize())
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap_or_else(|error| panic!("create {}: {error}", dst.display()));
    let entries = std::fs::read_dir(src).unwrap_or_else(|error| panic!("read {}: {error}", src.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("read package entry: {error}"));
        let target = dst.join(entry.file_name());
        let kind = entry.file_type().unwrap_or_else(|error| panic!("package entry type: {error}"));
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            if let Err(error) = std::fs::copy(entry.path(), &target) {
                panic!("copy {}: {error}", target.display());
            }
        } else {
            panic!("package entry is not a regular file: {}", entry.path().display());
        }
    }
}

fn write_tar_gz(package_dir: &Path, destination: &Path, top: &str) -> std::io::Result<()> {
    let file = std::fs::File::create(destination)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    // GNU sparse members are not regular files; the installer rejects them.
    builder.sparse(false);
    builder.append_dir_all(top, package_dir)?;
    builder.into_inner()?.finish()?;
    Ok(())
}

fn write_zip(package_dir: &Path, destination: &Path, top: &str) -> std::io::Result<()> {
    let file = std::fs::File::create(destination)?;
    let mut writer = zip::ZipWriter::new(file);
    zip_dir(&mut writer, package_dir, top)?;
    writer.finish()?;
    Ok(())
}

fn zip_dir(writer: &mut zip::ZipWriter<std::fs::File>, dir: &Path, prefix: &str) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "package path is not utf-8",
            ));
        };
        let rel = format!("{prefix}/{name}");
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            zip_dir(writer, &path, &rel)?;
        } else if kind.is_file() {
            let options = zip::write::SimpleFileOptions::default()
                .compression_level(Some(1))
                .unix_permissions(mode_bits(&path));
            writer.start_file(rel.as_str(), options)?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, writer)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "package entry is not a regular file",
            ));
        }
    }
    Ok(())
}

fn mode_bits(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).map(|meta| meta.permissions().mode() & 0o777).unwrap_or(0o644)
    }
    #[cfg(not(unix))]
    {
        if path.extension().and_then(|ext| ext.to_str()) == Some("exe") {
            0o755
        } else {
            0o644
        }
    }
}

struct ServedRelease {
    latest_path: String,
    asset_path: String,
    checksum_path: String,
    signature_path: String,
    signature: Vec<u8>,
    metadata: Vec<u8>,
    archive: Vec<u8>,
    checksum: Vec<u8>,
    requests: Arc<AtomicUsize>,
}

async fn serve_one(socket: &mut TcpStream, served: &ServedRelease) -> std::io::Result<()> {
    let path = tokio::time::timeout(HEADER_TIMEOUT, read_request_path(socket))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "release request headers timed out")
        })??;
    served.requests.fetch_add(1, Ordering::SeqCst);
    let (status, body) = if path == served.latest_path {
        ("200 OK", served.metadata.as_slice())
    } else if path == served.asset_path {
        ("200 OK", served.archive.as_slice())
    } else if path == served.checksum_path {
        ("200 OK", served.checksum.as_slice())
    } else if path == served.signature_path {
        ("200 OK", served.signature.as_slice())
    } else {
        ("404 Not Found", b"not found".as_slice())
    };
    write_http(socket, status, body).await
}

async fn read_request_path(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut received = Vec::new();
    let mut buf = [0u8; 1024];
    while !headers_complete(&received) {
        let count = socket.read(&mut buf).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "closed before HTTP headers",
            ));
        }
        received.extend_from_slice(&buf[..count]);
        if received.len() > 8192 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP headers exceed 8192 bytes",
            ));
        }
    }
    let text = String::from_utf8(received)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    request_path(&text).ok_or(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "HTTP request is not a GET",
    ))
}

fn headers_complete(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\r\n\r\n")
}

fn request_path(headers: &str) -> Option<String> {
    let mut parts = headers.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let path = parts.next()?.split('?').next()?;
    if path.is_empty() { None } else { Some(path.to_owned()) }
}

async fn write_http(socket: &mut TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.shutdown().await
}

async fn wait_for_id<R>(stdout: &mut tokio::io::Lines<R>, id: &str) -> bool
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match stdout.next_line().await {
            Ok(Some(line)) if response_id(&line, id) => return true,
            Ok(Some(_)) => continue,
            _ => return false,
        }
    }
}

fn response_id(line: &str, id: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else { return false };
    value.get("type").and_then(Value::as_str) == Some("response")
        && value.get("id").and_then(Value::as_str) == Some(id)
}
