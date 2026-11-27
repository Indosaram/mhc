//! Port of `packages/coding-agent/src/private-fd-startup-fillers.ts` (senpi#4602, #6282).

use std::collections::{BTreeMap, HashSet};

use super::private_fd_table::{
    DecodedPrivateFdTable, PRIVATE_FDS_TABLE_ENV, decode_private_fd_table,
};

pub const HOST_STARTUP_EVENTS_ENV: &str = "SENPI_HOST_STARTUP_EVENTS";
pub const STARTUP_EVENT_FD: u32 = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupHandOff {
    #[serde(default)]
    pub fillers: Vec<u32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdioSlot {
    Pipe,
    Ignore,
    Inherit,
    Fd(u32),
}

pub fn with_startup_pipe(base: &[Option<StdioSlot>]) -> (Vec<Option<StdioSlot>>, Vec<u32>) {
    let target_len = (STARTUP_EVENT_FD as usize + 1).max(base.len());
    let mut stdio = Vec::with_capacity(target_len);
    stdio.extend_from_slice(base);
    stdio.resize(target_len, None);

    let mut fillers = Vec::new();
    for slot in 3..STARTUP_EVENT_FD {
        let entry = &stdio[slot as usize];
        if entry.is_none() || *entry == Some(StdioSlot::Ignore) {
            stdio[slot as usize] = Some(StdioSlot::Pipe);
            fillers.push(slot);
        }
    }
    stdio[STARTUP_EVENT_FD as usize] = Some(StdioSlot::Pipe);
    (stdio, fillers)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FdIdentity {
    pub is_socket: bool,
    pub dev: u64,
    pub ino: u64,
}

pub trait FillerFdOps {
    fn platform(&self) -> &str;
    fn stat_fd(&self, fd: u32) -> Option<FdIdentity>;
    fn close_fd(&mut self, fd: u32) -> bool;
}

fn named_private_slots(encoded: Option<&str>) -> Option<HashSet<u32>> {
    let mut slots = HashSet::new();
    let Some(encoded) = encoded.filter(|s| !s.is_empty()) else {
        return Some(slots);
    };
    let DecodedPrivateFdTable::Parsed { values, .. } = decode_private_fd_table(encoded) else {
        return None;
    };
    for entry in values {
        match entry {
            serde_json::Value::Number(num) => {
                if let Some(fd) = num.as_u64().and_then(|v| u32::try_from(v).ok()) {
                    slots.insert(fd);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    if let Some(fd) = item.as_u64().and_then(|v| u32::try_from(v).ok()) {
                        slots.insert(fd);
                    }
                }
            }
            _ => {}
        }
    }
    Some(slots)
}

pub fn release_inherited_startup_fillers(
    env: &mut BTreeMap<String, String>,
    ops: &mut dyn FillerFdOps,
) {
    let Some(raw) = env.get(HOST_STARTUP_EVENTS_ENV) else {
        return;
    };
    let Ok(mut hand_off) = serde_json::from_str::<StartupHandOff>(raw) else {
        return;
    };
    if hand_off.fillers.is_empty() {
        return;
    }
    let private_slots = named_private_slots(env.get(PRIVATE_FDS_TABLE_ENV).map(String::as_str));
    if let Some(private_slots) = private_slots {
        let is_win32 = ops.platform() == "win32";
        let kept: Option<Vec<FdIdentity>> = if is_win32 {
            None
        } else {
            let startup = ops.stat_fd(STARTUP_EVENT_FD);
            startup.filter(|s| s.is_socket).map(|s| {
                let mut list = vec![s];
                for stdio_fd in 0..=2 {
                    if let Some(id) = ops.stat_fd(stdio_fd) {
                        list.push(id);
                    }
                }
                list
            })
        };
        for slot in &hand_off.fillers {
            if private_slots.contains(slot) {
                continue;
            }
            let is_filler = if is_win32 {
                true
            } else if let Some(kept) = &kept {
                ops.stat_fd(*slot).is_some_and(|stat| {
                    stat.is_socket
                        && !kept
                            .iter()
                            .any(|other| other.dev == stat.dev && other.ino == stat.ino)
                })
            } else {
                false
            };
            if is_filler {
                let _ = ops.close_fd(*slot);
            }
        }
    }
    hand_off.fillers.clear();
    if let Ok(encoded) = serde_json::to_string(&hand_off) {
        env.insert(HOST_STARTUP_EVENTS_ENV.to_owned(), encoded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockOps {
        platform: &'static str,
        stats: BTreeMap<u32, FdIdentity>,
        closed: Vec<u32>,
    }

    impl FillerFdOps for MockOps {
        fn platform(&self) -> &str {
            self.platform
        }
        fn stat_fd(&self, fd: u32) -> Option<FdIdentity> {
            self.stats.get(&fd).copied()
        }
        fn close_fd(&mut self, fd: u32) -> bool {
            self.closed.push(fd);
            self.stats.remove(&fd).is_some()
        }
    }

    #[test]
    fn fills_unused_slots_and_skips_private_and_non_filler_slots_on_release() {
        let base = vec![
            Some(StdioSlot::Inherit),
            Some(StdioSlot::Inherit),
            Some(StdioSlot::Inherit),
            None,
            Some(StdioSlot::Fd(4)),
        ];
        let (stdio, fillers) = with_startup_pipe(&base);
        assert_eq!(stdio[4], Some(StdioSlot::Fd(4)));
        assert_eq!(stdio[STARTUP_EVENT_FD as usize], Some(StdioSlot::Pipe));
        assert!(fillers.contains(&3) && !fillers.contains(&4));

        let mut env = BTreeMap::from([
            (
                HOST_STARTUP_EVENTS_ENV.to_owned(),
                r#"{"fillers":[3,5,6]}"#.to_owned(),
            ),
            (PRIVATE_FDS_TABLE_ENV.to_owned(), r#"{"priv":5}"#.to_owned()),
        ]);
        let mut ops = MockOps {
            platform: "linux",
            stats: BTreeMap::from([
                (
                    1,
                    FdIdentity {
                        is_socket: false,
                        dev: 1,
                        ino: 1,
                    },
                ),
                (
                    STARTUP_EVENT_FD,
                    FdIdentity {
                        is_socket: true,
                        dev: 2,
                        ino: 100,
                    },
                ),
                (
                    3,
                    FdIdentity {
                        is_socket: true,
                        dev: 2,
                        ino: 103,
                    },
                ),
                (
                    5,
                    FdIdentity {
                        is_socket: true,
                        dev: 2,
                        ino: 105,
                    },
                ),
                (
                    6,
                    FdIdentity {
                        is_socket: false,
                        dev: 1,
                        ino: 1,
                    },
                ),
            ]),
            closed: Vec::new(),
        };
        release_inherited_startup_fillers(&mut env, &mut ops);
        assert_eq!(ops.closed, vec![3]);
        assert!(!env[HOST_STARTUP_EVENTS_ENV].contains("[3,"));
    }
}
