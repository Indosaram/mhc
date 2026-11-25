//! How many clients a supervisor has accepted on its public socket, read from the operating system
//! (senpi `rpc/host-public-clients.ts`). An accepted unix stream socket carries the address of the
//! listener it was accepted on, so the supervisor's descriptors naming the public path are its listener
//! plus one per client. `None` means "could not be read", which every caller treats as "a client may be
//! there". Reading only.
use std::collections::HashSet;

/// `__SO_ACCEPTCON` in `/proc/net/unix` Flags: the socket is a listener.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LINUX_ACCEPTCON_FLAG: u64 = 0x10000;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LSOF_TIMEOUT_MS: u64 = 2_000;

struct PublicDescriptors {
    total: usize,
    listeners: usize,
}

#[derive(Clone, Copy)]
struct SocketDevIno {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn socket_dev_ino(meta: &std::fs::Metadata) -> SocketDevIno {
    use std::os::unix::fs::MetadataExt;
    SocketDevIno {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

/// Whether a reported socket path is the public socket: by name, or by resolving to the public entry's inode
/// (`<socket>.bind-*` scratch aliases share the same inode, senpi#5319).
struct PublicNameMatcher {
    names: HashSet<String>,
    published: Option<SocketDevIno>,
    seen: std::collections::HashMap<String, bool>,
}

impl PublicNameMatcher {
    fn new(socket: &str) -> Self {
        let mut names = HashSet::from([socket.to_owned()]);
        if let Ok(real) = std::fs::canonicalize(socket) {
            names.insert(real.to_string_lossy().into_owned());
        }
        #[cfg(unix)]
        let published = std::fs::symlink_metadata(socket)
            .ok()
            .map(|meta| socket_dev_ino(&meta));
        #[cfg(not(unix))]
        let published = None;
        Self {
            names,
            published,
            seen: std::collections::HashMap::new(),
        }
    }

    fn matches(&mut self, path: &str) -> bool {
        if self.names.contains(path) {
            return true;
        }
        let Some(published) = self.published else {
            return false;
        };
        if !path.starts_with('/') {
            return false;
        }
        if let Some(&known) = self.seen.get(path) {
            return known;
        }
        #[cfg(unix)]
        let same = std::fs::metadata(path).ok().is_some_and(|meta| {
            let entry = socket_dev_ino(&meta);
            entry.dev == published.dev && entry.ino == published.ino
        });
        #[cfg(not(unix))]
        let same = {
            let _ = published;
            false
        };
        self.seen.insert(path.to_owned(), same);
        same
    }
}

pub async fn count_public_clients(supervisor_pid: u32, socket: &str) -> Option<usize> {
    let mut matcher = PublicNameMatcher::new(socket);
    let descriptors = platform_descriptors(supervisor_pid, &mut matcher).await?;
    if descriptors.listeners == 0 {
        return None;
    }
    Some(descriptors.total - descriptors.listeners)
}

#[cfg(target_os = "linux")]
async fn platform_descriptors(
    pid: u32,
    matcher: &mut PublicNameMatcher,
) -> Option<PublicDescriptors> {
    let mut inodes = HashSet::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            inodes.insert(inode.to_owned());
        }
    }
    let table = std::fs::read_to_string("/proc/net/unix").ok()?;
    Some(count_linux_descriptors_with_matcher(
        &table, &inodes, matcher,
    ))
}

#[cfg(test)]
fn count_linux_descriptors(
    table: &str,
    inodes: &HashSet<String>,
    names: &HashSet<String>,
) -> PublicDescriptors {
    let mut matcher = PublicNameMatcher {
        names: names.clone(),
        published: None,
        seen: std::collections::HashMap::new(),
    };
    count_linux_descriptors_with_matcher(table, inodes, &mut matcher)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn count_linux_descriptors_with_matcher(
    table: &str,
    inodes: &HashSet<String>,
    matcher: &mut PublicNameMatcher,
) -> PublicDescriptors {
    let mut descriptors = PublicDescriptors {
        total: 0,
        listeners: 0,
    };
    for line in table.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(flags), Some(inode), Some(path)) = (fields.get(3), fields.get(6), fields.get(7))
        else {
            continue;
        };
        if !inodes.contains(*inode) || !matcher.matches(path) {
            continue;
        }
        descriptors.total += 1;
        if u64::from_str_radix(flags, 16).unwrap_or(0) & LINUX_ACCEPTCON_FLAG != 0 {
            descriptors.listeners += 1;
        }
    }
    descriptors
}

#[cfg(target_os = "macos")]
async fn platform_descriptors(
    pid: u32,
    matcher: &mut PublicNameMatcher,
) -> Option<PublicDescriptors> {
    let output = tokio::time::timeout(
        std::time::Duration::from_millis(LSOF_TIMEOUT_MS),
        tokio::process::Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-U", "-w", "-F", "n"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let total = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            line.strip_prefix('n')
                .is_some_and(|name| matcher.matches(name))
        })
        .count();
    // lsof does not mark the listening entry; a supervisor owns exactly one listener on its path.
    Some(PublicDescriptors {
        total,
        listeners: usize::from(total > 0),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn platform_descriptors(_pid: u32, _names: &HashSet<String>) -> Option<PublicDescriptors> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_and_clients_are_told_apart_by_the_acceptcon_flag() {
        let table = "Num RefCount Protocol Flags Type St Inode Path\n\
            0: 00000002 00000000 00010000 0001 01 100 /run/h.sock\n\
            0: 00000003 00000000 00000000 0001 03 101 /run/h.sock\n\
            0: 00000003 00000000 00000000 0001 03 102 /run/other.sock\n\
            0: 00000003 00000000 00000000 0001 03 999 /run/h.sock\n";
        let inodes = HashSet::from(["100".to_owned(), "101".to_owned(), "102".to_owned()]);
        let names = HashSet::from(["/run/h.sock".to_owned()]);
        let counted = count_linux_descriptors(table, &inodes, &names);
        assert_eq!((counted.total, counted.listeners), (2, 1));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn counts_one_accepted_client_of_this_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("h.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let socket = path.to_string_lossy().into_owned();
        assert_eq!(
            count_public_clients(std::process::id(), &socket).await,
            Some(0)
        );
        let _client = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        let (_accepted, _) = listener.accept().await.expect("accept");
        assert_eq!(
            count_public_clients(std::process::id(), &socket).await,
            Some(1)
        );
    }
}
