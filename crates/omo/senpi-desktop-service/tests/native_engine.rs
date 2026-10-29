use senpi_desktop_service::{DesktopService, DesktopServiceOptions, EngineSpawnConfig, engine_child_factory};

#[tokio::test]
async fn native_engine_opens_fake_backend_and_closes_at_stdin_eof() {
    let executable = std::env::current_exe().expect("test executable");
    let binary_name = format!("senpi-desktop-engine{}", std::env::consts::EXE_SUFFIX);
    let binary = executable.parent().expect("deps directory").parent().expect("target profile").join(binary_name);
    assert!(binary.is_file(), "build maho-senpi-desktop-engine before the service acceptance gate");
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../desktop/senpi-desktop-backend-fake/fixtures/two-displays-one-window.json");
    let mut config = EngineSpawnConfig::new(binary);
    config.env.insert("SENPI_DESKTOP_BACKEND".into(), format!("fake:{}", scenario.display()));
    let service = DesktopService::new(DesktopServiceOptions { create_child: Some(engine_child_factory(Some(config))), on_notification: None });
    let opened = service.open(Default::default()).await.expect("open native engine");
    assert_eq!(opened.backend, "fake");
    let capabilities = service.capabilities().await.expect("capabilities");
    assert_eq!(capabilities.display_count, 2);
    tokio::time::timeout(std::time::Duration::from_secs(5), service.close()).await.expect("bounded close").expect("close");
}
