pub mod kill_point;
pub mod receipts;

pub use kill_point::{maybe_kill_at, KillPointOptions, MemoryKillPoint, MEMORY_KILL_POINTS};
pub use receipts::{
    append_memory_receipt, append_memory_receipt_once, read_memory_receipts, receipt_identity,
    receipts_path, MemoryReceipt, MemoryReceiptEvent, MemoryReceiptIdentityInput,
    MemoryReceiptInput, MemoryReceiptKind, MemoryReceiptsRead, ReadMemoryReceiptsOptions,
};

#[cfg(test)]
mod tests;
