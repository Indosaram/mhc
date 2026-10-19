use maho_omo_config_resolution::load_senpi_omo_config;
use omo_config_core::LoadOmoConfigOptions;
use serde_json::{Value, json};

fn resolve(config: Value, profile: Option<&str>) -> Result<maho_omo_config_resolution::SenpiOmoConfigResult, std::io::Error> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(home.join(".omo"))?;
    std::fs::create_dir_all(&project)?;
    std::fs::write(home.join(".omo/omo.jsonc"), config.to_string())?;
    let home_str = omo_config_core::to_posix_path(&home.to_string_lossy());
    let mut env = std::collections::BTreeMap::from([
        ("HOME".into(), home_str.clone()),
        ("USERPROFILE".into(), home_str),
    ]);
    if let Some(profile) = profile { env.insert("OMO_PROFILE".into(), profile.into()); }
    Ok(load_senpi_omo_config(LoadOmoConfigOptions { cwd: Some(omo_config_core::to_posix_path(&project.to_string_lossy())), env: Some(env), platform: Some("linux".into()), ..Default::default() }))
}

#[test]
fn preserves_top_level_configuration() -> Result<(), std::io::Error> {
    let result = resolve(json!({"categories":{"quick":{"model":"provider/fast","reasoningEffort":"minimal"},"deep-low":{"fallback_models":["provider/deep"]}},"agents":{"explore":{"model":"provider/explore","models":["provider/other"]},"oracle":{"model":"provider/oracle","reasoningEffort":"max"}}}), None)?;
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(result.config["categories"], json!({"quick":{"model":"provider/fast","reasoning":"minimal"},"deep-low":{"fallback_models":["provider/deep"]}}));
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

#[test]
fn malformed_config_reports_diagnostic_without_panicking() -> Result<(), std::io::Error> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(project.join(".omo"))?;
    let config = project.join(".omo/omo.jsonc");
    std::fs::write(&config, "{\"task\":")?;
    let home_str = omo_config_core::to_posix_path(&home.to_string_lossy());
    let config_str = omo_config_core::to_posix_path(&config.to_string_lossy());
    let result = load_senpi_omo_config(LoadOmoConfigOptions {
        cwd: Some(omo_config_core::to_posix_path(&project.to_string_lossy())),
        env: Some(std::collections::BTreeMap::from([
            ("HOME".into(), home_str.clone()),
            ("USERPROFILE".into(), home_str),
        ])),
        platform: Some("linux".into()), ..Default::default()
    });
    assert!(result.diagnostics.iter().any(|diagnostic| matches!(diagnostic,
        maho_omo_config_resolution::SenpiConfigDiagnostic::Config(diagnostic)
            if diagnostic.kind == "parse" && diagnostic.path == config_str)));
    Ok(())
}
