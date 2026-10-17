#[derive(Clone, Debug)]
pub struct KernelRunInput {
    pub cell_id: String,
    pub code: String,
    pub timeout_ms: Option<u64>,
}
