use maho_codemode::bridge::protocol::LocalRoots;
use maho_codemode::kernels::js::local_module_loader::*;
use std::path::PathBuf;

/// `file://` URL for a rooted test path: a drive-less root resolves against the current drive on
/// Windows (`/tmp` -> `file:///D:/tmp/` when the cwd is on `D:`).
fn root_url(path: &str) -> String {
    if cfg!(windows) {
        let cwd = std::env::current_dir().expect("cwd");
        let drive = cwd.to_string_lossy().chars().next().expect("drive letter");
        format!("file:///{drive}:{path}")
    } else {
        format!("file://{path}")
    }
}

#[test]
fn case_fold_collision_uses_last_inserted_root() {
    for object in [
        r#"{"LOCAL":"/tmp/first","local":"/tmp/last"}"#,
        r#"{"local":"/tmp/first","LOCAL":"/tmp/last"}"#,
    ] {
        let roots: LocalRoots = serde_json::from_str(object).unwrap();
        let options = LocalModuleLoaderOptions {
            cwd: "/tmp".into(),
            local_roots: Some(roots),
            artifacts_dir: None,
        };
        let expected = root_url("/tmp/last/");
        assert_eq!(
            runtime_context(&options).unwrap()["localRootUrls"]["local"],
            expected
        );
        let bridge = local_bridge_connection(&options);
        let wire = serde_json::to_value(bridge).unwrap();
        assert_eq!(wire["localRoots"]["local"], "/tmp/last");
    }
}

#[test]
fn context_normalizes_roots_and_preserves_explicit_local_root() {
    let options = LocalModuleLoaderOptions {
        cwd: PathBuf::from("/tmp/project/../with space"),
        local_roots: Some(LocalRoots::from([("LOCAL".into(), "/tmp/explicit".into())])),
        artifacts_dir: Some("/tmp/artifacts".into()),
    };
    let context = runtime_context(&options).unwrap();
    let expected_cwd = root_url("/tmp/with%20space/");
    let expected_explicit = root_url("/tmp/explicit/");
    assert_eq!(context["cwdUrl"], expected_cwd);
    assert_eq!(context["localRootUrls"]["local"], expected_explicit);
    let bridge = local_bridge_connection(&options);
    assert_eq!(bridge.port, 1);
    assert_eq!(bridge.token, "local");
}

#[test]
fn cell_payload_contains_prepared_sentinel_and_rewritten_code() {
    let options = LocalModuleLoaderOptions {
        cwd: "/tmp/project".into(),
        local_roots: None,
        artifacts_dir: Some("/tmp/artifacts".into()),
    };
    let expected = root_url("/tmp/artifacts/local/");
    assert_eq!(
        runtime_context(&options).unwrap()["localRootUrls"]["local"],
        expected
    );
    let prepared = LocalModuleLoader::new(&options)
        .unwrap()
        .prepare_cell("import value from './value.mjs';", &[]);
    let payload: serde_json::Value =
        serde_json::from_str(prepared.strip_prefix(PREPARED_CELL_PREFIX).unwrap()).unwrap();
    assert_eq!(
        payload["code"],
        "const value = (await __senpi_import__(\"./value.mjs\")).default;"
    );
    assert!(
        payload["prelude"]
            .as_str()
            .unwrap()
            .contains("globalThis.__senpi_import__")
    );
}

#[tokio::test]
async fn prepared_imports_execute_in_external_worker() {
    use maho_codemode::kernels::{
        js::context_manager::JavaScriptKernel, shared::subprocess_contract::KernelRunInput,
    };
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let loader = LocalModuleLoader::new(&LocalModuleLoaderOptions {
        cwd: cwd.into(),
        local_roots: None,
        artifacts_dir: None,
    })
    .unwrap();
    let kernel = JavaScriptKernel::start(cwd, "prepared-import", 4, None)
        .await
        .unwrap();
    let result = kernel
        .run(
            KernelRunInput {
                cell_id: "import".into(),
                code: loader.prepare_cell(
                    "import { basename } from 'node:path'; basename('/tmp/value');",
                    &[],
                ),
                timeout_ms: Some(5000),
            },
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["valueRepr"], "\"value\"");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn local_import_rejects_encoded_traversal_and_recovers() {
    use maho_codemode::{
        bridge::protocol::BridgeConnectionConfig,
        kernels::{
            js::context_manager::JavaScriptKernel, shared::subprocess_contract::KernelRunInput,
        },
    };
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let kernel = JavaScriptKernel::start_with_connection(
        cwd,
        "local-traversal",
        4,
        None,
        BridgeConnectionConfig {
            port: 1,
            token: "test".into(),
            local_roots: Some(LocalRoots::from([(
                "local".into(),
                cwd.to_string_lossy().into_owned(),
            )])),
            artifacts_dir: None,
            parallel_pool_width: None,
        },
    )
    .await
    .unwrap();
    let mut failures = Vec::new();
    for specifier in [
        "local://../outside.mjs",
        "local://%2e%2e/outside.mjs",
        "local:///absolute.mjs",
        "local://%ZZ",
        "unsupported://module.mjs",
    ] {
        let code = format!(
            "await import({})",
            serde_json::to_string(specifier).unwrap()
        );
        failures.push(
            kernel
                .run(
                    KernelRunInput {
                        cell_id: "bad-import".into(),
                        code,
                        timeout_ms: Some(5000),
                    },
                    |_| {},
                )
                .await
                .unwrap(),
        );
    }
    let recovered = kernel
        .run(
            KernelRunInput {
                cell_id: "after-import".into(),
                code: "42".into(),
                timeout_ms: Some(5000),
            },
            |_| {},
        )
        .await
        .unwrap();
    kernel.close().await.unwrap();
    for failure in failures {
        assert_eq!(failure["ok"], false);
    }
    assert_eq!(recovered["valueRepr"], "42");
}

#[tokio::test]
async fn escaped_static_sources_execute_in_actual_worker() {
    use maho_codemode::kernels::{
        js::context_manager::JavaScriptKernel, shared::subprocess_contract::KernelRunInput,
    };
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let kernel = JavaScriptKernel::start(cwd, "escaped-import", 4, None)
        .await
        .unwrap();
    let result = kernel
        .run(
            KernelRunInput {
                cell_id: "escaped-import".into(),
                code: r"import { basename } from 'node:\x70\u0061th'; basename('/tmp/answer');"
                    .into(),
                timeout_ms: Some(5000),
            },
            |_| {},
        )
        .await;
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());
    eprintln!("cleanup: escaped-import worker closed; pid None");
    let result = result.unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["valueRepr"], "\"answer\"");
}

#[tokio::test]
async fn file_url_imports_and_unsupported_scheme_refusal() {
    use maho_codemode::kernels::{
        js::context_manager::JavaScriptKernel, shared::subprocess_contract::KernelRunInput,
    };
    let dir = tempfile::tempdir().expect("temp dir");
    let fixture = dir.path().join("answer.mjs");
    std::fs::write(&fixture, "export const answer = 42;\n").expect("write fixture");
    let canonical = std::fs::canonicalize(&fixture).expect("canonical fixture");
    let fixture_url = format!("file://{}", canonical.display());

    let kernel = JavaScriptKernel::start(dir.path(), "file-url-import", 4, None)
        .await
        .expect("kernel");
    let static_import = kernel
        .run(
            KernelRunInput {
                cell_id: "file-url-static".into(),
                code: format!("import {{ answer }} from \"{fixture_url}\";\nanswer;"),
                timeout_ms: Some(5000),
            },
            |_| {},
        )
        .await
        .expect("static import");
    let refused = kernel
        .run(
            KernelRunInput {
                cell_id: "bad-scheme".into(),
                code: "await import(\"nosuchroot://fixture.mjs\");".into(),
                timeout_ms: Some(5000),
            },
            |_| {},
        )
        .await
        .expect("refused import");
    kernel.close().await.expect("close");

    assert_eq!(static_import["ok"], true, "{static_import}");
    assert_eq!(static_import["valueRepr"], "42");
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Unsupported module protocol: nosuchroot://fixture.mjs"),
        "{refused}"
    );
}
