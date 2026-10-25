//! Revisioned guidance custody (`pause/guidance.ts`).
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuidanceScope {
    Run,
    Node {
        #[serde(rename = "nodeId")]
        node_id: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceState {
    Draft,
    Dispatching,
    Delivered,
    Removed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub task_id: String,
    pub entry_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guidance {
    pub id: String,
    pub revision: u64,
    pub scope: GuidanceScope,
    pub text: String,
    pub state: GuidanceState,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub deliveries: Vec<Delivery>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GuidanceError {
    #[error("not_found")]
    NotFound,
    #[error("frozen")]
    Frozen,
    #[error("duplicate_id")]
    DuplicateId,
}
pub fn add_guidance(
    records: &[Guidance],
    id: String,
    scope: GuidanceScope,
    text: String,
    created_at: String,
) -> Result<Vec<Guidance>, GuidanceError> {
    if records.iter().any(|r| r.id == id) {
        return Err(GuidanceError::DuplicateId);
    }
    let mut records = records.to_vec();
    records.push(Guidance {
        id,
        revision: 0,
        scope,
        text,
        created_at,
        edited_at: None,
        state: GuidanceState::Draft,
        deliveries: vec![],
    });
    Ok(records)
}
pub fn change_guidance(
    records: &[Guidance],
    id: &str,
    text: Option<String>,
    remove: bool,
    edited_at: String,
) -> Result<Vec<Guidance>, GuidanceError> {
    let current = records
        .iter()
        .find(|r| r.id == id)
        .ok_or(GuidanceError::NotFound)?;
    if current.state != GuidanceState::Draft {
        return Err(GuidanceError::Frozen);
    }
    let mut records = records.to_vec();
    for record in &mut records {
        if record.id != id {
            continue;
        }
        record.revision += 1;
        record.edited_at = Some(edited_at.clone());
        if let Some(text) = &text {
            record.text.clone_from(text);
        }
        record.state = if remove {
            GuidanceState::Removed
        } else {
            GuidanceState::Draft
        };
    }
    Ok(records)
}
pub struct GuidanceTarget {
    pub node_id: String,
    pub task_id: Option<String>,
}
pub trait GuidancePorts {
    type Error;
    fn save(&mut self, records: &[Guidance]) -> Result<(), Self::Error>;
    fn deliver(
        &mut self,
        task_id: &str,
        input: super::ports::HoldInput,
    ) -> Result<Option<String>, Self::Error>;
}
pub fn dispatch_guidance<P: GuidancePorts>(
    initial: &[Guidance],
    targets: &[GuidanceTarget],
    ports: &mut P,
) -> Result<(), P::Error> {
    let mut records = initial.to_vec();
    let mut ordered = initial.to_vec();
    ordered.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    for mut guidance in ordered {
        if matches!(
            guidance.state,
            GuidanceState::Removed | GuidanceState::Delivered
        ) {
            continue;
        }
        guidance.state = GuidanceState::Dispatching;
        save(&mut records, &guidance, ports)?;
        let selected: Vec<_> = targets
            .iter()
            .filter(|t| match &guidance.scope {
                GuidanceScope::Run => true,
                GuidanceScope::Node { node_id } => node_id == &t.node_id,
            })
            .collect();
        for target in &selected {
            let Some(task_id) = &target.task_id else {
                continue;
            };
            if guidance.deliveries.iter().any(|d| &d.task_id == task_id) {
                continue;
            }
            let entry_id = ports.deliver(
                task_id,
                super::ports::HoldInput {
                    input_id: guidance.id.clone(),
                    revision: guidance.revision,
                    text: guidance.text.clone(),
                },
            )?;
            guidance.deliveries.push(Delivery {
                task_id: task_id.clone(),
                entry_id,
            });
            save(&mut records, &guidance, ports)?;
        }
        if selected.iter().all(|t| {
            t.task_id
                .as_ref()
                .is_some_and(|id| guidance.deliveries.iter().any(|d| &d.task_id == id))
        }) {
            guidance.state = GuidanceState::Delivered;
            save(&mut records, &guidance, ports)?;
        }
    }
    Ok(())
}
fn save<P: GuidancePorts>(
    records: &mut [Guidance],
    record: &Guidance,
    ports: &mut P,
) -> Result<(), P::Error> {
    for entry in records.iter_mut() {
        if entry.id == record.id {
            *entry = record.clone();
        }
    }
    ports.save(records)
}
