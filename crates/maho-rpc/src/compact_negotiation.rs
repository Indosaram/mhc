//! senpi `modes/rpc/compact-negotiation.ts`: per-connection capability negotiation for
//! `compact_message_updates_v1` + `event_resync` and G1 `set_client_info` reply shaping.

use crate::connection_roles::CONNECTION_ROLE_FIXED;
use crate::socket_peer_recovery::EVENT_RESYNC_CAPABILITY;
use maho_core::engine_socket_flag::compact_updates_enabled;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const COMPACT_EVENTS_CAPABILITY: &str = "compact_events";
pub const COMPACT_UPDATES_CAPABILITY: &str = "compact_message_updates_v1";
pub const COMPACT_PAIR: [&str; 2] = [COMPACT_UPDATES_CAPABILITY, EVENT_RESYNC_CAPABILITY];
pub const DELEGATION_INCARNATION_STALE: &str = "delegation_incarnation_stale";

/// A declaration belongs to one actual socket registration, never a session or launch profile.
#[derive(Debug, Default, Clone)]
pub struct CompactSelection {
    pub capabilities: Vec<String>,
    pub selected: bool,
    pub admitted: bool,
    pub refused: bool,
}

impl CompactSelection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn declare(
        &mut self,
        next: Option<&[String]>,
        supported: bool,
        roles: impl FnOnce(&[String]) -> bool,
    ) -> Option<&'static str> {
        self.declare_with_flag(next, supported, roles, compact_updates_enabled())
    }

    pub fn declare_with_flag(
        &mut self,
        next: Option<&[String]>,
        supported: bool,
        roles: impl FnOnce(&[String]) -> bool,
        enabled: bool,
    ) -> Option<&'static str> {
        if self.refused {
            return Some("compact_selection_refused");
        }
        let caps: Vec<String> = match next {
            Some(n) => n.to_vec(),
            None => self.capabilities.clone(),
        };
        let compact = enabled && caps.iter().any(|c| c == COMPACT_UPDATES_CAPABILITY);
        let pair = compact && caps.iter().any(|c| c == EVENT_RESYNC_CAPABILITY);
        let refusal = if compact && !pair {
            Some("compact_pair_required")
        } else if compact && !supported {
            Some("compact_endpoint_unsupported")
        } else if (self.selected && !pair) || (self.admitted && pair != self.selected) {
            Some("compact_selection_fixed")
        } else {
            None
        };
        if let Some(refusal) = refusal {
            if !self.selected && compact && !self.admitted {
                self.refused = true;
            }
            return Some(refusal);
        }
        if next.is_some() && !roles(&caps) {
            if compact && !self.selected && !self.admitted {
                self.refused = true;
            }
            return Some("connection_role_fixed");
        }
        self.capabilities = caps
            .into_iter()
            .filter(|cap| {
                cap != COMPACT_EVENTS_CAPABILITY && (enabled || cap != COMPACT_UPDATES_CAPABILITY)
            })
            .collect();
        self.selected = pair;
        None
    }

    pub fn wire_capabilities(&self, caps: &[String], enabled: bool) -> Vec<String> {
        if !enabled {
            caps.iter()
                .filter(|cap| cap.as_str() != COMPACT_EVENTS_CAPABILITY)
                .cloned()
                .collect()
        } else if self.selected {
            let mut out = caps.to_vec();
            out.push(COMPACT_EVENTS_CAPABILITY.into());
            out
        } else {
            caps.to_vec()
        }
    }

    pub fn ack(&self) -> Option<Value> {
        self.selected
            .then(|| json!({ "capabilities": COMPACT_PAIR }))
    }
}

/// Unknown/inherited opt-ins cannot stand in for an actual endpoint implementation.
pub fn advertised_compact_capabilities(caps: &[String], supported: bool) -> Vec<String> {
    advertised_compact_capabilities_with_flag(caps, supported, compact_updates_enabled())
}

pub fn advertised_compact_capabilities_with_flag(
    caps: &[String],
    supported: bool,
    enabled: bool,
) -> Vec<String> {
    if !enabled {
        return caps.to_vec();
    }
    let mut legacy: Vec<String> = caps
        .iter()
        .filter(|cap| {
            cap.as_str() != COMPACT_UPDATES_CAPABILITY && cap.as_str() != COMPACT_EVENTS_CAPABILITY
        })
        .cloned()
        .collect();
    if supported {
        let mut seen: BTreeSet<String> = legacy.iter().cloned().collect();
        for cap in COMPACT_PAIR {
            if seen.insert(cap.to_owned()) {
                legacy.push(cap.to_owned());
            }
        }
    }
    legacy
}

pub fn declaration_reply(record: Value, selection: Option<&CompactSelection>) -> Option<Value> {
    declaration_reply_with_flag(record, selection, compact_updates_enabled())
}

pub fn declaration_reply_with_flag(
    mut record: Value,
    selection: Option<&CompactSelection>,
    enabled: bool,
) -> Option<Value> {
    if record.get("type").and_then(Value::as_str) != Some("response")
        || record.get("command").and_then(Value::as_str) != Some("set_client_info")
    {
        return Some(record);
    }
    if record.get("id").is_none() || record["id"].is_null() {
        return if enabled { None } else { Some(record) };
    }
    if record.get("success").and_then(Value::as_bool) == Some(true)
        && let Some(ack) = selection.and_then(CompactSelection::ack)
    {
        record["data"] = ack;
    }
    Some(record)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleDeclarationVerdict {
    Declared,
    Fixed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDeclarationResult {
    pub capabilities: Option<Vec<String>>,
    pub refusal: Option<&'static str>,
    pub grant_changed: bool,
}

pub fn session_declaration(
    has_client_info_channel: bool,
    command_capabilities: Option<&[String]>,
    current: Option<&[String]>,
    declare: impl FnOnce(&[String], Option<u64>) -> RoleDeclarationVerdict,
    set_client_info_caps: impl FnOnce(&[String], Option<u64>) -> RoleDeclarationVerdict,
    replace: impl FnOnce(&[String]),
    incarnation: Option<u64>,
) -> SessionDeclarationResult {
    let had_grant = current.is_some_and(|c| c.iter().any(|x| x == "grant_select"));
    if let Some(caps) = command_capabilities {
        if !has_client_info_channel
            && compact_updates_enabled()
            && caps.iter().any(|c| c == COMPACT_UPDATES_CAPABILITY)
        {
            return SessionDeclarationResult {
                capabilities: current.map(<[String]>::to_vec),
                refusal: Some("compact_endpoint_unsupported"),
                grant_changed: false,
            };
        }
        match declare(caps, incarnation) {
            RoleDeclarationVerdict::Stale => {
                return SessionDeclarationResult {
                    capabilities: current.map(<[String]>::to_vec),
                    refusal: Some(DELEGATION_INCARNATION_STALE),
                    grant_changed: false,
                };
            }
            RoleDeclarationVerdict::Fixed => {
                return SessionDeclarationResult {
                    capabilities: current.map(<[String]>::to_vec),
                    refusal: Some(CONNECTION_ROLE_FIXED),
                    grant_changed: false,
                };
            }
            RoleDeclarationVerdict::Declared => {}
        }
        if !has_client_info_channel {
            replace(caps);
        } else if set_client_info_caps(caps, incarnation) == RoleDeclarationVerdict::Stale {
            return SessionDeclarationResult {
                capabilities: current.map(<[String]>::to_vec),
                refusal: Some(DELEGATION_INCARNATION_STALE),
                grant_changed: false,
            };
        }
    }
    let next_caps = command_capabilities.or(current);
    let has_grant = next_caps.is_some_and(|c| c.iter().any(|x| x == "grant_select"));
    SessionDeclarationResult {
        capabilities: next_caps.map(<[String]>::to_vec),
        refusal: None,
        grant_changed: had_grant != has_grant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_compact_pair_and_sticks_refusal_before_admission() {
        let mut sel = CompactSelection::new();
        assert_eq!(
            sel.declare_with_flag(
                Some(&[COMPACT_UPDATES_CAPABILITY.into()]),
                true,
                |_| true,
                true,
            ),
            Some("compact_pair_required")
        );
        assert!(sel.refused);
        assert_eq!(
            sel.declare_with_flag(
                Some(&[
                    COMPACT_UPDATES_CAPABILITY.into(),
                    EVENT_RESYNC_CAPABILITY.into()
                ]),
                true,
                |_| true,
                true,
            ),
            Some("compact_selection_refused")
        );

        let mut sel2 = CompactSelection::new();
        assert_eq!(
            sel2.declare_with_flag(
                Some(&[
                    COMPACT_UPDATES_CAPABILITY.into(),
                    EVENT_RESYNC_CAPABILITY.into()
                ]),
                true,
                |_| true,
                true,
            ),
            None
        );
        assert!(sel2.selected);
        assert!(sel2.ack().is_some());
        assert!(
            sel2.wire_capabilities(&["question".into()], true)
                .contains(&COMPACT_EVENTS_CAPABILITY.to_owned())
        );
        assert!(
            !sel2
                .wire_capabilities(&[COMPACT_EVENTS_CAPABILITY.into()], false)
                .contains(&COMPACT_EVENTS_CAPABILITY.to_owned())
        );
    }

    #[test]
    fn advertised_and_declaration_reply_honor_feature_flag() {
        let caps = vec![
            "question".into(),
            COMPACT_UPDATES_CAPABILITY.into(),
            COMPACT_EVENTS_CAPABILITY.into(),
        ];
        assert_eq!(
            advertised_compact_capabilities_with_flag(&caps, true, false),
            caps
        );
        let adv = advertised_compact_capabilities_with_flag(&caps, true, true);
        assert!(adv.contains(&COMPACT_UPDATES_CAPABILITY.to_owned()));
        assert!(adv.contains(&EVENT_RESYNC_CAPABILITY.to_owned()));
        assert!(!adv.contains(&COMPACT_EVENTS_CAPABILITY.to_owned()));

        let idless = json!({"type":"response","command":"set_client_info","success":true});
        assert!(declaration_reply_with_flag(idless.clone(), None, true).is_none());
        assert!(declaration_reply_with_flag(idless, None, false).is_some());
    }
}
