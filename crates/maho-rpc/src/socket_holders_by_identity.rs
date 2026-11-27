//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-holders-by-identity.ts`.
//!
//! The processes holding a unix socket, found by the socket's inode instead of its name (#5319).
//!
//! `None` means "could not be read", which every caller treats as "a holder may be there".

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

const SCAN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketIdentity {
    pub dev: u64,
    pub ino: u64,
}

fn strip_lsof_socket_type(raw: &str) -> &str {
    match raw.find(" type=") {
        Some(idx) => {
            let after = &raw[idx + 6..];
            if after.is_empty() || after.starts_with(char::is_whitespace) {
                raw
            } else {
                &raw[..idx]
            }
        }
        None => raw,
    }
}

pub async fn parse_lsof_holders_with_stat<F, Fut>(
    listing: &str,
    identity: SocketIdentity,
    mut stat_identity: F,
) -> Vec<u32>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Option<SocketIdentity>>,
{
    let mut pids = HashSet::new();
    let mut order = Vec::new();
    let mut same_inode: HashMap<String, bool> = HashMap::new();
    let mut current_pid: Option<u32> = None;

    for line in listing.split('\n') {
        if let Some(rest) = line.strip_prefix('p') {
            current_pid = rest.trim().parse::<u32>().ok();
            continue;
        }
        let Some(pid) = current_pid else {
            continue;
        };
        if !line.starts_with("n/") {
            continue;
        }
        let name = strip_lsof_socket_type(&line[1..]).to_owned();
        let same = match same_inode.get(&name) {
            Some(&cached) => cached,
            None => {
                let matched = stat_identity(name.clone())
                    .await
                    .is_some_and(|entry| entry == identity);
                same_inode.insert(name, matched);
                matched
            }
        };
        if same && pids.insert(pid) {
            order.push(pid);
        }
    }
    order
}

#[cfg(unix)]
async fn stat_socket_dev_ino(path: String) -> Option<SocketIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = tokio::fs::metadata(Path::new(&path)).await.ok()?;
    Some(SocketIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
async fn stat_socket_dev_ino(_path: String) -> Option<SocketIdentity> {
    None
}

pub async fn socket_holders_by_identity(identity: SocketIdentity) -> Option<Vec<u32>> {
    let mut cmd = tokio::process::Command::new("lsof");
    cmd.args(["-U", "-w", "-F", "pn"]);
    let output = tokio::time::timeout(SCAN_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    // lsof exits 1 when some process could not be read; the listing it did print is still evidence.
    let code = output.status.code();
    if code != Some(0) && code != Some(1) {
        return None;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Some(parse_lsof_holders_with_stat(&listing, identity, stat_socket_dev_ino).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_lsof_output_strips_type_suffix_and_deduplicates_pids() {
        let listing = "\
p100
n/run/senpi/other.sock type=STREAM
n/run/senpi/a.bind-1/s type=STREAM
p200
n/run/senpi/s
p100
n/run/senpi/s
";
        let target = SocketIdentity { dev: 10, ino: 42 };
        let pids = parse_lsof_holders_with_stat(listing, target, |path| async move {
            if path == "/run/senpi/a.bind-1/s" || path == "/run/senpi/s" {
                Some(SocketIdentity { dev: 10, ino: 42 })
            } else {
                Some(SocketIdentity { dev: 10, ino: 99 })
            }
        })
        .await;
        assert_eq!(pids, vec![100, 200]);
    }
}
