use std::{collections::BTreeMap, sync::Arc};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn real_startup_migration_matches_reference_document() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-migration.json")).expect("reference");
    let expected = &reference["entries"].as_array().expect("entries").iter().find(|entry| entry["type"] == "qa_migration_document").expect("document")["document"];
    let home = tempfile::tempdir().expect("private home");
    std::fs::create_dir(home.path().join(".maho")).expect("config dir");
    std::fs::write(home.path().join(".maho/config.jsonc"), r#"{"codegraph":{"daemon":false}}"#).expect("legacy config");
    let home_path = home.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([("HOME".into(), home_path.clone())]);
    let loader_env = env.clone();
    let component = maho_omo_config_startup::ConfigStartupComponent {
        run_migration: Some(Arc::new(move |cwd| maho_omo_config_startup::run_senpi_startup_migration(cwd, &env, &home_path))),
        load_config: Some(Arc::new(move |cwd| maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions { cwd: Some(cwd.into()), env: Some(loader_env.clone()), ..Default::default() }))),
    };
    let session = FauxSession::new(load_script("hello").expect("script")).with_native_extension(NativeExtensionFactory { path: "<startup>".into(), source_info: Default::default(), extension: Box::new(component) });
    tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded run").expect("session");
    let document = std::fs::read_to_string(home.path().join(".maho/omo.jsonc")).expect("migrated document");
    let actual = omo_config_core::internal::jsonc::parse_jsonc_safe(&document);
    assert!(actual.errors.is_empty());
    assert_eq!(actual.data.expect("jsonc document"), *expected);
}
