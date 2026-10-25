//! Run fences and inherited holds on the shared session ledger.
use crate::engine_turn_hold::{EngineTurnHold, HoldKind, HoldReleaseCause};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRef {
    pub kind: String,
    pub id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InheritedData {
    pub parent_session_id: String,
    pub run_ref: RunRef,
    pub high_water: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum InheritResult {
    Inherited { child_seq: u64 },
    WrongParent,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseInheritedResult {
    Released { other_hold_live: bool },
    Stale,
    WrongParent,
    NotHeld,
}
fn fence_key(run_ref: &RunRef) -> String {
    format!("run_fence:{}:{}", run_ref.kind, run_ref.id)
}
pub struct RunHolds<'a> {
    ledger: &'a mut EngineTurnHold,
}
impl<'a> RunHolds<'a> {
    pub fn new(ledger: &'a mut EngineTurnHold) -> Self {
        Self { ledger }
    }
    pub fn fence(&mut self, run_ref: &RunRef) {
        self.ledger.add_hold(
            HoldKind::RunFence,
            Some(&fence_key(run_ref)),
            Some(json!({"runRef":run_ref})),
        );
    }
    pub fn release_fences(
        &mut self,
        cause: HoldReleaseCause,
        run_refs: Option<&[RunRef]>,
    ) -> Result<(), String> {
        let keys: Vec<String> = run_refs
            .map(|refs| refs.iter().map(fence_key).collect())
            .unwrap_or_else(|| {
                self.ledger
                    .holds()
                    .into_iter()
                    .filter(|h| h.kind == HoldKind::RunFence)
                    .map(|h| h.key)
                    .collect()
            });
        self.ledger.release(&keys, cause)
    }
    pub fn holds_wake(&self, details: &Value) -> bool {
        details
            .get("runRef")
            .and_then(|v| serde_json::from_value::<RunRef>(v.clone()).ok())
            .is_some_and(|r| self.ledger.get_hold(&fence_key(&r)).is_some())
    }
    pub fn pause_seq(&self) -> u64 {
        self.ledger.current_pause_seq()
    }

    pub fn inherited_owner(&self) -> Option<InheritedData> {
        serde_json::from_value(self.ledger.get_hold("inherited_hold")?.data?).ok()
    }
    pub fn stamp(&mut self) {
        let seq = self.ledger.current_pause_seq() + 1;
        self.ledger.add_hold(
            HoldKind::SessionHold,
            Some(&format!("session_hold:{seq}")),
            Some(json!({"localSeq":seq})),
        );
    }
    pub fn release_direct_stops(&mut self) -> Result<(), String> {
        let keys = self
            .ledger
            .holds()
            .iter()
            .filter(|h| h.kind == HoldKind::SessionHold)
            .map(|h| h.key.clone())
            .collect::<Vec<_>>();
        self.ledger.release(&keys, HoldReleaseCause::Input)
    }
    pub fn inherit(
        &mut self,
        parent_session_id: &str,
        parent_pause_seq: u64,
        run_ref: RunRef,
    ) -> Result<InheritResult, String> {
        let current = self.inherited_owner();
        if current
            .as_ref()
            .is_some_and(|owner| owner.parent_session_id != parent_session_id)
        {
            return Ok(InheritResult::WrongParent);
        }
        let high_water = current
            .as_ref()
            .map_or(0, |owner| owner.high_water)
            .max(parent_pause_seq);
        if current.is_some() {
            self.ledger
                .release(&["inherited_hold".into()], HoldReleaseCause::RunResume)?;
        }
        self.ledger.add_hold(
            HoldKind::InheritedHold,
            Some("inherited_hold"),
            Some(json!(InheritedData {
                parent_session_id: parent_session_id.into(),
                run_ref,
                high_water
            })),
        );
        Ok(InheritResult::Inherited {
            child_seq: self.ledger.current_pause_seq(),
        })
    }
    pub fn release_inherited(
        &mut self,
        parent_session_id: &str,
        parent_pause_seq: u64,
        child_seq_cutoff: u64,
    ) -> Result<ReleaseInheritedResult, String> {
        let Some(owner) = self.inherited_owner() else {
            return Ok(ReleaseInheritedResult::NotHeld);
        };
        if owner.parent_session_id != parent_session_id {
            return Ok(ReleaseInheritedResult::WrongParent);
        }
        if owner.high_water != parent_pause_seq {
            return Ok(ReleaseInheritedResult::Stale);
        }
        let direct: Vec<_> = self
            .ledger
            .holds()
            .into_iter()
            .filter(|h| h.kind == HoldKind::SessionHold)
            .collect();
        let mut keys = vec!["inherited_hold".into()];
        keys.extend(
            direct
                .iter()
                .filter(|h| {
                    h.data
                        .as_ref()
                        .and_then(|v| v.get("localSeq"))
                        .and_then(Value::as_u64)
                        .is_some_and(|seq| seq <= child_seq_cutoff)
                })
                .map(|h| h.key.clone()),
        );
        let other_hold_live = direct.len() > keys.len() - 1;
        self.ledger.release(&keys, HoldReleaseCause::RunResume)?;
        Ok(ReleaseInheritedResult::Released { other_hold_live })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn ledger() -> EngineTurnHold {
        EngineTurnHold::new(
            &[],
            false,
            Arc::new(|_, _| Ok(())),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
    }
    #[test]
    fn inherits_high_water_and_rejects_stale_or_wrong_parent() {
        let mut ledger = ledger();
        let mut holds = RunHolds::new(&mut ledger);
        let run = RunRef {
            kind: "task".into(),
            id: "run".into(),
        };
        assert!(matches!(
            holds.inherit("parent", 4, run.clone()),
            Ok(InheritResult::Inherited { .. })
        ));
        holds.inherit("parent", 2, run.clone()).expect("inherit");
        assert_eq!(holds.inherited_owner().expect("owner").high_water, 4);
        assert_eq!(
            holds.inherit("other", 8, run),
            Ok(InheritResult::WrongParent)
        );
        assert_eq!(
            holds.release_inherited("parent", 2, u64::MAX),
            Ok(ReleaseInheritedResult::Stale)
        );
        holds.stamp();
        let cutoff = holds.ledger.current_pause_seq();
        holds.stamp();
        assert_eq!(
            holds.release_inherited("parent", 4, cutoff),
            Ok(ReleaseInheritedResult::Released {
                other_hold_live: true
            })
        );
        assert_eq!(holds.ledger.holds().len(), 1);
    }
    #[test]
    fn fences_only_matching_run_wakes() {
        let mut ledger = ledger();
        let mut holds = RunHolds::new(&mut ledger);
        let run = RunRef {
            kind: "dag".into(),
            id: "r".into(),
        };
        holds.fence(&run);
        assert!(holds.holds_wake(&json!({"runRef":run})));
        assert!(!holds.holds_wake(&json!({"runRef":{"kind":"dag","id":"other"}})));
        holds
            .release_fences(HoldReleaseCause::RunResume, None)
            .expect("release");
        assert!(!holds.holds_wake(&json!({"runRef":run})));
    }
}
