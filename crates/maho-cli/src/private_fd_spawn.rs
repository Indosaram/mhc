//! Port of `packages/coding-agent/src/private-fd-spawn.ts` (senpi#4749).

use std::collections::BTreeMap;

use super::private_fd_startup_fillers::StdioSlot;
use super::private_fd_table::{PRIVATE_FDS_TABLE_ENV, UNPARSEABLE_TABLE};
use super::private_fds::{PRIVATE_FDS_FINAL_ENV, PrivateFdEntry, PrivateFdForward};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedPrivateFdSpawn {
    pub env: BTreeMap<String, String>,
    pub stdio: Vec<Option<StdioSlot>>,
    pub owned_fds: Vec<u32>,
}

/// Transfers the captured table only on explicit engine spawns, never via ambient child environments.
/// A shell cannot carry a descriptor, so it receives the name as a null declaration.
/// `final_hop` marks the supervisor-to-engine hop: that engine claims its channels and forwards nothing.
pub fn prepare_private_fd_spawn(
    base_env: &BTreeMap<String, String>,
    base_stdio: &[Option<StdioSlot>],
    uses_shell: bool,
    final_hop: bool,
    forward: &PrivateFdForward,
) -> PreparedPrivateFdSpawn {
    let mut env = base_env.clone();
    env.remove(PRIVATE_FDS_TABLE_ENV);
    env.remove(PRIVATE_FDS_FINAL_ENV);
    let mut stdio = base_stdio.to_vec();
    let mut owned_fds = Vec::new();
    match forward {
        PrivateFdForward::Unparseable => {
            env.insert(
                PRIVATE_FDS_TABLE_ENV.to_owned(),
                UNPARSEABLE_TABLE.to_owned(),
            );
        }
        PrivateFdForward::Entries(entries) if !entries.is_empty() => {
            let carries = !uses_shell;
            let mut table = BTreeMap::new();
            for (name, entry) in entries {
                let descriptors: Vec<u32> = match entry {
                    PrivateFdEntry::Null => Vec::new(),
                    PrivateFdEntry::Single(fd) => vec![*fd],
                    PrivateFdEntry::Pair([a, b]) => vec![*a, *b],
                };
                owned_fds.extend(&descriptors);
                if *entry != PrivateFdEntry::Null && carries {
                    for descriptor in descriptors {
                        let slot = descriptor as usize;
                        if stdio.len() <= slot {
                            stdio.resize(slot + 1, Some(StdioSlot::Ignore));
                        }
                        stdio[slot] = Some(StdioSlot::Fd(descriptor));
                    }
                    table.insert(name.clone(), entry.clone());
                } else {
                    table.insert(name.clone(), PrivateFdEntry::Null);
                }
            }
            if let Ok(json) = serde_json::to_string(&table) {
                env.insert(PRIVATE_FDS_TABLE_ENV.to_owned(), json);
            }
        }
        PrivateFdForward::Entries(_) => {}
    }
    if env.contains_key(PRIVATE_FDS_TABLE_ENV) && final_hop {
        env.insert(PRIVATE_FDS_FINAL_ENV.to_owned(), "1".to_owned());
    }
    PreparedPrivateFdSpawn {
        env,
        stdio,
        owned_fds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_spawns_null_declarations_while_direct_spawns_forward_slots() {
        let forward = PrivateFdForward::Entries(BTreeMap::from([(
            "presenter".to_owned(),
            PrivateFdEntry::Single(5),
        )]));
        let direct = prepare_private_fd_spawn(&BTreeMap::new(), &[], false, true, &forward);
        assert_eq!(direct.stdio.get(5).copied(), Some(Some(StdioSlot::Fd(5))));
        assert_eq!(
            direct.env.get(PRIVATE_FDS_FINAL_ENV).map(String::as_str),
            Some("1")
        );
        assert_eq!(direct.owned_fds, vec![5]);

        let shell = prepare_private_fd_spawn(&BTreeMap::new(), &[], true, false, &forward);
        assert!(shell.stdio.is_empty());
        assert_eq!(
            shell.env.get(PRIVATE_FDS_TABLE_ENV).map(String::as_str),
            Some(r#"{"presenter":null}"#)
        );
    }
}
