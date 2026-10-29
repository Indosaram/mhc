//! `dag/owner.ts`: the DAG ownership stamp carried by owned task starts.
//!
//! The owned start result union (`fresh` / `reused` / non-started / `owner_conflict`) is the
//! manager's [`OwnedStartResult`]; the owner stamp itself ([`DagTaskOwner`]) is the manager's own
//! canonical type (`crate::shared::DagTaskOwner`, already wired through `start_owned`), so this
//! module only re-exports both from the module that defines the DAG owner contract instead of
//! duplicating a second, incompatible shape.

pub use crate::manager::types::OwnedStartResult;
pub use crate::shared::{DagOwnerKind, DagTaskOwner, DagTaskOwnerKey};

/// The `kind: "dag"` literal of every DAG task owner.
pub const DAG_TASK_OWNER_KIND: &str = "dag";
