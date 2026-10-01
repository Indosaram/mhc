use maho_omo_config_resolution::load_senpi_omo_config;
use omo_config_core::LoadOmoConfigOptions;
use serde_json::{Value, json};

fn resolve(config: Value, profile: Option<&str>) -> Result<maho_omo_config_resolution::SenpiOmoConfigResult, std::io::Error> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::create_dir_all(&project)?;
    std::fs::write(home.join(".maho/omo.jsonc"), config.to_string())?;
    let mut env = std::collections::BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]);
    if let Some(profile) = profile { env.insert("OMO_PROFILE".into(), profile.into()); }
    Ok(load_senpi_omo_config(LoadOmoConfigOptions { cwd: Some(project.to_string_lossy().into_owned()), env: Some(env), platform: Some("linux".into()), ..Default::default() }))
}

#[test]
fn preserves_top_level_configuration() -> Result<(), std::io::Error> {
    let result = resolve(json!({"categories":{"quick":{"model":"provider/fast","reasoningEffort":"minimal"},"deep":{"fallback_models":["provider/deep"]}},"agents":{"explore":{"model":"provider/explore","models":["provider/other"]},"oracle":{"model":"provider/oracle","reasoningEffort":"max"}}}), None)?;
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.config["categories"], json!({"quick":{"model":"provider/fast","reasoning":"minimal"},"deep":{"fallback_models":["provider/deep"]}}));
    assert_eq!(result.config["agents"], json!({"explore":{"model":"provider/explore","models":["provider/other"]},"oracle":{"model":"provider/oracle","reasoning":"max"}}));
    Ok(())
}

#[test]
fn activated_senpi_profile_overrides_base() -> Result<(), std::io::Error> {
    let result = resolve(json!({"categories":{"quick":{"model":"base/model"}},"[senpi]":{"categories":{"quick":{"model":"senpi/model"}}},"profiles":{"focused":{"[senpi]":{"categories":{"quick":{"model":"profile/model"}}}}}}), Some("focused"))?;
    assert_eq!(result.loaded.profile.as_deref(), Some("focused"));
    assert_eq!(result.config["categories"]["quick"]["model"], "profile/model");
    Ok(())
}

#[test]
fn expands_catalog_and_inherits_attributes() -> Result<(), std::io::Error> {
    let result = resolve(json!({"models":{"fast":{"model":"provider/fast","reasoningEffort":"low","variant":"rapid"}},"categories":{"quick":{"fallback_models":["fast"],"model":"fast"}},"agents":{"finder":{"model":"fast","models":["fast"]}}}), None)?;
    assert_eq!(result.config["categories"]["quick"], json!({"fallback_models":[{"model":"provider/fast","reasoning":"low"}],"model":"provider/fast","reasoning":"low"}));
    assert_eq!(result.config["agents"]["finder"], json!({"model":"provider/fast","models":[{"model":"provider/fast","reasoning":"low"}],"reasoning":"low"}));
    Ok(())
}
