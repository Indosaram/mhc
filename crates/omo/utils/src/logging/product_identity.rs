#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductIdentityInput {
    pub plugin_name: String,
    pub legacy_plugin_name: String,
    pub published_package_name: String,
    pub config_basename: String,
    pub legacy_config_basename: String,
    pub log_file_name: String,
    pub cache_dir_name: String,
    pub accepted_package_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductIdentity {
    pub plugin_name: String,
    pub legacy_plugin_name: String,
    pub published_package_name: String,
    pub accepted_package_names: Vec<String>,
    pub config_basename: String,
    pub legacy_config_basename: String,
    pub log_file_name: String,
    pub cache_dir_name: String,
}

pub fn create_product_identity(input: ProductIdentityInput) -> ProductIdentity {
    let accepted_package_names = input.accepted_package_names.unwrap_or_else(|| {
        vec![
            input.published_package_name.clone(),
            input.plugin_name.clone(),
        ]
    });
    ProductIdentity {
        plugin_name: input.plugin_name,
        legacy_plugin_name: input.legacy_plugin_name,
        published_package_name: input.published_package_name,
        accepted_package_names,
        config_basename: input.config_basename,
        legacy_config_basename: input.legacy_config_basename,
        log_file_name: input.log_file_name,
        cache_dir_name: input.cache_dir_name,
    }
}
