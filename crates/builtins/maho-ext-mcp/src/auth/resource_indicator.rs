pub fn get_resource_indicator(value: &str) -> Result<String, url::ParseError> {
    let mut parsed = url::Url::parse(value)?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    let mut normalized = parsed.to_string();
    if normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

pub fn add_resource_to_params(params: &mut Vec<(String, String)>, resource: &str) {
    let mut replaced = false;
    params.retain_mut(|(key, value)| {
        if key != "resource" {
            return true;
        }
        if replaced {
            return false;
        }
        resource.clone_into(value);
        replaced = true;
        true
    });
    if !replaced {
        params.push(("resource".into(), resource.into()));
    }
}
