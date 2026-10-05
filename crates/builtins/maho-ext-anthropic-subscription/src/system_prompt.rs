pub fn resolve_custom_prompt(prompt:Option<&str>)->String {prompt.unwrap_or_default().into()}
fn guidance(path:Option<&str>,reason:&str)->String {
    let target=path.map_or_else(||"systemPromptFile".into(),|p|format!("systemPromptFile \"{p}\""));
    format!("Anthropic Subscription override prompt could not load {target}: {reason}. Set claudeSdkOauthProvider.systemPromptFile to a readable, non-empty UTF-8 prompt file, or select systemPromptMode \"full\".")
}
pub fn load_override_prompt(path:Option<&str>)->anyhow::Result<String> {
    let Some(path)=path else {anyhow::bail!(guidance(None,"the path is not configured"));};
    let content=std::fs::read_to_string(path).map_err(|error|anyhow::anyhow!(guidance(Some(path),&error.to_string())))?;
    if content.trim().is_empty() {anyhow::bail!(guidance(Some(path),"the file is empty"));}Ok(resolve_custom_prompt(Some(&content)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_content_and_rejects_empty_missing_files() {
        assert_eq!(resolve_custom_prompt(None),"");let directory=tempfile::tempdir().expect("directory");let path=directory.path().join("prompt");let spelling=path.to_str().expect("path");
        assert!(load_override_prompt(None).is_err());assert!(load_override_prompt(Some(spelling)).is_err());std::fs::write(&path," \n ").expect("empty");assert!(load_override_prompt(Some(spelling)).is_err());std::fs::write(&path," keep boundaries\n").expect("prompt");assert_eq!(load_override_prompt(Some(spelling)).expect("load")," keep boundaries\n");
    }
}
