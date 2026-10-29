pub const CATEGORY_CONFIG_SOURCES: &[&str] = &["startup", "reload", "new", "resume", "fork"];

pub fn category_property_name(name: &str) -> String { format!("cat_{}", name.replace('-', "_")) }
pub static CATEGORY_CONFIG_PROPERTY_NAMES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| crate::product_identity::builtin_category_names().into_iter().map(category_property_name).collect());
pub static CATEGORY_CONFIG_SCHEMA: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| serde_json::json!({
    "$session_id": {"type":"string"},
    "builtin_overridden_count": {"type":"number"},
    "cat_architect": {"type":"string"},
    "cat_artistry": {"type":"string"},
    "cat_deep_high": {"type":"string"},
    "cat_deep_low": {"type":"string"},
    "cat_quick": {"type":"string"},
    "cat_ultrabrain": {"type":"string"},
    "cat_unspecified_high": {"type":"string"},
    "cat_unspecified_low": {"type":"string"},
    "cat_visual_engineering": {"type":"string"},
    "cat_writing": {"type":"string"},
    "combo_fingerprint": {"type":"string"},
    "config_generation": {"type":"number"},
    "source": {"type":"string", "values": CATEGORY_CONFIG_SOURCES},
    "user_category_count": {"type":"number"}
}));
