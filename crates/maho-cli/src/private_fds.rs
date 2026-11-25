//! Port of `packages/coding-agent/src/private-fds.ts` (senpi#4749, #4898).

use std::collections::{BTreeMap, BTreeSet};

use super::private_fd_startup_fillers::{FillerFdOps, release_inherited_startup_fillers};
use super::private_fd_table::{
    DecodedPrivateFdTable, PRIVATE_FDS_TABLE_ENV, decode_private_fd_table,
};

/// Set only on the supervisor-to-engine hop: the engine claims its channels and never forwards them.
pub const PRIVATE_FDS_FINAL_ENV: &str = "SENPI_PRIVATE_FDS_FINAL";
/// POSIX adopts only 5..=15; Windows pairs instead clear inheritance explicitly at receipt.
pub const ADOPTABLE_MAX: u32 = 15;
pub const WINDOWS_PRIVATE_FD_PAIR_PROVEN_BUN: &str = "1.4.4";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum PrivateFdEntry {
    Single(u32),
    Pair([u32; 2]),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrivateFdForward {
    Entries(BTreeMap<String, PrivateFdEntry>),
    Unparseable,
}

pub trait PrivateFdInspection {
    fn platform(&self) -> &str;
    fn bun_version(&self) -> Option<&str>;
    fn proven_bun(&self) -> Option<&str> {
        Some(WINDOWS_PRIVATE_FD_PAIR_PROVEN_BUN)
    }
    /// `Some(None)` = no IPC descriptor; `Some(Some(fd))` = known IPC fd; `None` = IPC exists with unknown fd.
    fn ipc(&self) -> Option<Option<u32>> {
        Some(None)
    }
    fn is_socket(&self, fd: u32) -> bool;
    fn is_pipe(&self, fd: u32) -> bool;
    fn clear_inherit(&self, fd: u32) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPrivateFdTable {
    pub entries: BTreeMap<String, PrivateFdEntry>,
    pub parsed: bool,
    pub discard: BTreeSet<u32>,
}

fn as_valid_fd(value: &serde_json::Value) -> Option<u32> {
    let raw = value.as_u64()?;
    let fd = u32::try_from(raw).ok()?;
    (5..=64).contains(&fd).then_some(fd)
}

fn raw_numbers(value: &serde_json::Value) -> Vec<u32> {
    match value {
        serde_json::Value::Number(num) => num
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .into_iter()
            .collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_u64().and_then(|v| u32::try_from(v).ok()))
            .collect(),
        _ => Vec::new(),
    }
}

fn inspect_entry(
    value: &serde_json::Value,
    shared: bool,
    runtime: &dyn PrivateFdInspection,
    cleared: &BTreeMap<u32, bool>,
) -> (PrivateFdEntry, Option<u32>) {
    let Some(ipc) = runtime.ipc() else {
        return (PrivateFdEntry::Null, None);
    };
    if let serde_json::Value::Array(items) = value {
        if runtime.platform() != "win32"
            || runtime.proven_bun().is_none()
            || runtime.bun_version() != runtime.proven_bun()
            || items.len() != 2
        {
            return (PrivateFdEntry::Null, None);
        }
        let (Some(fd0), Some(fd1)) = (as_valid_fd(&items[0]), as_valid_fd(&items[1])) else {
            return (PrivateFdEntry::Null, None);
        };
        if fd0 == fd1
            || shared
            || ipc == Some(fd0)
            || ipc == Some(fd1)
            || !runtime.is_pipe(fd0)
            || !runtime.is_pipe(fd1)
        {
            return (PrivateFdEntry::Null, None);
        }
        if cleared.get(&fd0).copied() == Some(true) && cleared.get(&fd1).copied() == Some(true) {
            return (PrivateFdEntry::Pair([fd0, fd1]), None);
        }
        return (PrivateFdEntry::Null, None);
    }
    let Some(fd) = as_valid_fd(value) else {
        return (PrivateFdEntry::Null, None);
    };
    if runtime.platform() == "win32" || ipc == Some(fd) || !runtime.is_socket(fd) {
        return (PrivateFdEntry::Null, None);
    }
    if shared || fd > ADOPTABLE_MAX {
        return (PrivateFdEntry::Null, Some(fd));
    }
    (PrivateFdEntry::Single(fd), None)
}

pub fn parse_table(encoded: &str, runtime: &dyn PrivateFdInspection) -> ParsedPrivateFdTable {
    let decoded = decode_private_fd_table(encoded);
    let (raw, all_values) = match decoded {
        DecodedPrivateFdTable::Unparsed { names } => {
            let entries = names
                .into_iter()
                .map(|name| (name, PrivateFdEntry::Null))
                .collect();
            return ParsedPrivateFdTable {
                entries,
                parsed: false,
                discard: BTreeSet::new(),
            };
        }
        DecodedPrivateFdTable::Parsed { entries, values } => (entries, values),
    };
    let mut cleared = BTreeMap::new();
    if runtime.platform() == "win32" {
        for value in &all_values {
            for fd in raw_numbers(value) {
                if (5..=64).contains(&fd) && !cleared.contains_key(&fd) {
                    cleared.insert(fd, runtime.clear_inherit(fd));
                }
            }
        }
    }
    let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
    for (_, value) in &raw {
        for fd in raw_numbers(value) {
            *counts.entry(fd).or_insert(0) += 1;
        }
    }
    let mut entries = BTreeMap::new();
    let mut discard = BTreeSet::new();
    for (name, value) in raw {
        let shared = raw_numbers(&value)
            .into_iter()
            .any(|fd| counts.get(&fd).copied().unwrap_or(0) > 1);
        let (channel, discarded) = inspect_entry(&value, shared, runtime, &cleared);
        entries.insert(name, channel);
        if let Some(fd) = discarded {
            discard.insert(fd);
        }
    }
    for (fd, ok) in cleared {
        if !ok && runtime.ipc().flatten() != Some(fd) {
            discard.insert(fd);
        }
    }
    ParsedPrivateFdTable {
        entries,
        parsed: true,
        discard,
    }
}

#[derive(Clone, Debug)]
pub struct PrivateFdState {
    pub present: bool,
    pub declared: BTreeSet<String>,
    pub unparseable: bool,
    pub channels: BTreeMap<String, PrivateFdEntry>,
    pub forward: BTreeMap<String, PrivateFdEntry>,
    pub forward_unparseable: bool,
    pub discard: BTreeSet<u32>,
}

impl PrivateFdState {
    pub fn capture_from_env(
        env: &mut BTreeMap<String, String>,
        is_main_thread: bool,
        inspection: &dyn PrivateFdInspection,
        filler_ops: &mut dyn FillerFdOps,
    ) -> Self {
        if is_main_thread {
            release_inherited_startup_fillers(env, filler_ops);
        }
        let encoded = if is_main_thread {
            env.remove(PRIVATE_FDS_TABLE_ENV)
        } else {
            None
        };
        let final_hop =
            is_main_thread && env.remove(PRIVATE_FDS_FINAL_ENV).is_some_and(|v| v == "1");
        let parsed_table = match &encoded {
            Some(text) => parse_table(text, inspection),
            None => ParsedPrivateFdTable {
                entries: BTreeMap::new(),
                parsed: true,
                discard: BTreeSet::new(),
            },
        };
        let unparseable = !parsed_table.parsed && parsed_table.entries.is_empty();
        let declared = parsed_table.entries.keys().cloned().collect();
        let mut channels = BTreeMap::new();
        let mut forward = BTreeMap::new();
        for (name, entry) in parsed_table.entries {
            if entry != PrivateFdEntry::Null {
                channels.insert(name.clone(), entry.clone());
            }
            if !final_hop {
                forward.insert(name, entry);
            }
        }
        Self {
            present: encoded.is_some(),
            declared,
            unparseable,
            channels,
            forward,
            forward_unparseable: unparseable && !final_hop,
            discard: parsed_table.discard,
        }
    }

    pub fn private_channel_declared(&self, name: &str) -> bool {
        self.declared.contains(name)
    }

    pub fn take_channel(&mut self, name: &str) -> Option<PrivateFdEntry> {
        let entry = self.channels.remove(name)?;
        if self.forward.contains_key(name) {
            self.forward.insert(name.to_owned(), PrivateFdEntry::Null);
        }
        Some(entry)
    }

    pub fn take_private_descriptors(&mut self) -> PrivateFdForward {
        if self.forward_unparseable {
            return PrivateFdForward::Unparseable;
        }
        let entries = self.forward.clone();
        for (name, entry) in &entries {
            self.forward.insert(name.clone(), PrivateFdEntry::Null);
            if *entry != PrivateFdEntry::Null {
                self.channels.remove(name);
            }
        }
        PrivateFdForward::Entries(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubInspect {
        platform: &'static str,
        bun: Option<&'static str>,
        sockets: BTreeSet<u32>,
        pipes: BTreeSet<u32>,
    }

    impl PrivateFdInspection for StubInspect {
        fn platform(&self) -> &str {
            self.platform
        }
        fn bun_version(&self) -> Option<&str> {
            self.bun
        }
        fn is_socket(&self, fd: u32) -> bool {
            self.sockets.contains(&fd)
        }
        fn is_pipe(&self, fd: u32) -> bool {
            self.pipes.contains(&fd)
        }
        fn clear_inherit(&self, _fd: u32) -> bool {
            true
        }
    }

    struct NoopFillers;
    impl FillerFdOps for NoopFillers {
        fn platform(&self) -> &str {
            "linux"
        }
        fn stat_fd(
            &self,
            _fd: u32,
        ) -> Option<super::super::private_fd_startup_fillers::FdIdentity> {
            None
        }
        fn close_fd(&mut self, _fd: u32) -> bool {
            false
        }
    }

    #[test]
    fn enforces_range_uniqueness_single_claim_and_final_hop() {
        let inspect = StubInspect {
            platform: "linux",
            bun: None,
            sockets: [5, 6, 7, 20].into_iter().collect(),
            pipes: BTreeSet::new(),
        };
        let mut env = BTreeMap::from([(
            PRIVATE_FDS_TABLE_ENV.to_owned(),
            r#"{"good":5,"high":20,"dup-a":6,"dup-b":6}"#.to_owned(),
        )]);
        let mut state =
            PrivateFdState::capture_from_env(&mut env, true, &inspect, &mut NoopFillers);
        assert!(state.present);
        assert!(!env.contains_key(PRIVATE_FDS_TABLE_ENV));
        assert!(state.discard.contains(&20));
        assert_eq!(state.take_channel("good"), Some(PrivateFdEntry::Single(5)));
        assert_eq!(state.take_channel("good"), None);
        assert_eq!(state.take_channel("dup-a"), None);
        assert_eq!(state.take_channel("high"), None);
    }
}
