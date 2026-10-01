pub fn resolve_grep_engine() -> Result<(), crate::definition::ToolError> {
    let requested = std::env::var("SENPI_GREP_ENGINE").unwrap_or_else(|_| "auto".into());
    if !["auto","native","rg"].contains(&requested.as_str()) { return Err(crate::definition::ToolError::Message(format!("Unknown SENPI_GREP_ENGINE value: {requested}"))); }
    super::native_loader::verify_native_abi()
}
