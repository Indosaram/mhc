pub const NATIVE_GREP_ABI_VERSION: &str = "1";
pub fn verify_native_abi() -> Result<(), crate::definition::ToolError> {
    if maho_grep::senpi_grep_abi_sentinel() == NATIVE_GREP_ABI_VERSION { Ok(()) }
    else { Err(crate::definition::ToolError::Message("Native grep ABI mismatch: expected __senpiGrepAbi1() === \"1\" and a grep function".into())) }
}
