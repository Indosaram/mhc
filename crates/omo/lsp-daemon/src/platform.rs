use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Operating-system family the daemon paths and transport branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Unix,
    Windows,
}

/// A platform capability this port does not provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedPlatform(pub &'static str);

impl fmt::Display for UnsupportedPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unsupported platform for {}", self.0)
    }
}

impl std::error::Error for UnsupportedPlatform {}

pub const fn current_kind() -> PlatformKind {
    if cfg!(windows) {
        PlatformKind::Windows
    } else {
        PlatformKind::Unix
    }
}

pub fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

pub fn tmp_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows\\Temp"))
    } else {
        std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    }
}

pub fn username() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|value| !value.is_empty())
}

#[cfg(unix)]
pub fn current_uid() -> Option<u32> {
    Some(unix_uid())
}

#[cfg(unix)]
fn unix_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: `getuid` is a side-effect-free libc call that always succeeds and returns the
    // real uid of the calling process. It cannot fail and cannot touch memory we own.
    unsafe { getuid() }
}

#[cfg(not(unix))]
pub fn current_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill` with signal 0 performs permission and existence checks only; it sends no
    // signal and mutates no memory. The return value is an errno indicator.
    let result = unsafe { kill(pid, 0) };
    if result == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
pub fn process_alive(pid: u32) -> bool {
    pid != 0
}

/// Sends `signal` to `pid`; `true` when the kernel accepted it (TS `process.kill` not throwing).
#[cfg(unix)]
pub fn send_signal(pid: u32, signal: i32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` only inspects its integer arguments; it does not touch process memory.
    unsafe { libc::kill(pid, signal) == 0 }
}

#[cfg(not(unix))]
pub fn send_signal(_pid: u32, _signal: i32) -> bool {
    false
}

pub fn random_bytes(len: usize) -> io::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut file = std::fs::File::open("/dev/urandom")?;
        let mut buffer = vec![0_u8; len];
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }
    #[cfg(not(unix))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            UnsupportedPlatform("cryptographically secure random bytes"),
        ))
    }
}

#[cfg(unix)]
pub fn set_private_file_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn set_private_file_mode(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn set_socket_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn set_socket_mode(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn open_directory_following_nothing(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
pub fn open_directory_following_nothing(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(unix)]
pub fn directory_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub fn directory_identity(_file: &std::fs::File) -> io::Result<(u64, u64)> {
    Ok((0, 0))
}

#[cfg(unix)]
pub fn path_identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub fn path_identity(_path: &Path) -> io::Result<(u64, u64)> {
    Ok((0, 0))
}

#[cfg(unix)]
pub fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(not(unix))]
pub fn is_socket(_path: &Path) -> bool {
    false
}

#[cfg(unix)]
pub type SocketStream = std::os::unix::net::UnixStream;

#[cfg(unix)]
pub type SocketListener = std::os::unix::net::UnixListener;

#[cfg(not(unix))]
#[derive(Debug)]
pub struct SocketStream {
    _private: (),
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct SocketListener {
    _private: (),
}

#[cfg(unix)]
pub fn connect_socket(path: &Path) -> io::Result<SocketStream> {
    std::os::unix::net::UnixStream::connect(path)
}

#[cfg(not(unix))]
pub fn connect_socket(_path: &Path) -> io::Result<SocketStream> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        UnsupportedPlatform("unix domain sockets"),
    ))
}

#[cfg(unix)]
pub fn bind_socket(path: &Path) -> io::Result<SocketListener> {
    std::os::unix::net::UnixListener::bind(path)
}

#[cfg(not(unix))]
pub fn bind_socket(_path: &Path) -> io::Result<SocketListener> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        UnsupportedPlatform("unix domain sockets"),
    ))
}

#[cfg(not(unix))]
impl io::Read for SocketStream {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            UnsupportedPlatform("unix domain sockets"),
        ))
    }
}

#[cfg(not(unix))]
impl io::Write for SocketStream {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            UnsupportedPlatform("unix domain sockets"),
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
