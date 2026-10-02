use serde_json::Value;

pub struct NativeImageGenModel<'a> {
    pub id: &'a str, pub provider: &'a str, pub api: &'a str, pub base_url: &'a str, pub compat: Option<&'a Value>,
}
pub fn is_enabled(value: Option<&str>) -> bool {
    !value.is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
}
pub fn is_open_ai_image_gen_enabled() -> bool {
    is_enabled(std::env::var("PI_OPENAI_IMAGE_GEN").ok().as_deref())
}
pub fn supports_native_image_generation(target: Option<&NativeImageGenModel<'_>>) -> bool {
    let Some(target) = target else { return false; };
    if target.api != "openai-responses" { return false; }
    if let Some(value) = target.compat.and_then(|compat| compat.get("supportsImageGeneration")).and_then(Value::as_bool) { return value; }
    let base_url = if target.base_url.is_empty() { "https://api.openai.com/v1" } else { target.base_url };
    url::Url::parse(base_url).is_ok_and(|url| url.host_str() == Some("api.openai.com"))
}
pub fn native_image_gen_model_key(target: Option<&NativeImageGenModel<'_>>) -> String {
    target.map_or_else(String::new, |target| format!("{}|{}|{}|{}", target.provider, target.api, target.base_url, target.id))
}
