use super::host::{HandleRef, HandleSnapshot, HandlePhase};
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum PoolStatus { Open, Closing, Completed, Error, Cancelled }
#[derive(Debug, Clone)] pub struct PoolRecord { pub id: String, pub owner_session_id: String, pub status: PoolStatus, pub item_count: usize }
pub fn settled(pool: &PoolRecord) -> bool { matches!(pool.status, PoolStatus::Completed|PoolStatus::Error|PoolStatus::Cancelled) || matches!(pool.status, PoolStatus::Closing) && pool.item_count == 0 }
pub fn snapshot(pool: &PoolRecord, reference: HandleRef) -> HandleSnapshot { HandleSnapshot { reference, phase: if pool.status == PoolStatus::Cancelled { HandlePhase::Cancelled } else if settled(pool) { HandlePhase::Succeeded } else { HandlePhase::Pending }, host_status: format!("{:?}", pool.status), revision: i64::from(settled(pool)) } }
