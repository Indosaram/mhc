pub mod kill_point;
pub mod owner;
#[allow(clippy::module_inception)] // mirrors the TS receipts/receipts.ts layout
pub mod receipts;

pub use kill_point::{KillPointOptions, MEMORY_KILL_POINTS, MemoryKillPoint, maybe_kill_at};
pub use receipts::{
    MemoryReceipt, MemoryReceiptEvent, MemoryReceiptIdentityInput, MemoryReceiptInput,
    MemoryReceiptKind, MemoryReceiptsRead, ReadMemoryReceiptsOptions, append_memory_receipt,
    append_memory_receipt_once, read_memory_receipts, receipt_identity, receipts_path,
};

#[cfg(test)]
mod tests;
