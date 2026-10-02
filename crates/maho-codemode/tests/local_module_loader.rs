use std::{collections::HashMap, path::PathBuf};
use maho_codemode::kernels::js::local_module_loader::*;

#[test]
fn context_normalizes_roots_and_preserves_explicit_local_root() {
    let options = LocalModuleLoaderOptions {cwd:PathBuf::from("/tmp/project/../with space"), local_roots:Some(HashMap::from([("LOCAL".into(), "/tmp/explicit".into())])), artifacts_dir:Some("/tmp/artifacts".into())};
    let context = runtime_context(&options).unwrap();
    assert_eq!(context["cwdUrl"], "file:///tmp/with%20space/");
    assert_eq!(context["localRootUrls"]["local"], "file:///tmp/explicit/");
    let bridge = local_bridge_connection(&options);
    assert_eq!(bridge.port, 1);
    assert_eq!(bridge.token, "local");
}

#[test]
fn cell_payload_contains_prepared_sentinel_and_rewritten_code() {
    let options = LocalModuleLoaderOptions {cwd:"/tmp/project".into(),local_roots:None,artifacts_dir:Some("/tmp/artifacts".into())};
    assert_eq!(runtime_context(&options).unwrap()["localRootUrls"]["local"], "file:///tmp/artifacts/local/");
    let prepared = LocalModuleLoader::new(&options).unwrap().prepare_cell("import value from './value.mjs';");
    let payload:serde_json::Value = serde_json::from_str(prepared.strip_prefix(PREPARED_CELL_PREFIX).unwrap()).unwrap();
    assert_eq!(payload["code"], "const value = (await __senpi_import__(\"./value.mjs\")).default;");
    assert!(payload["prelude"].as_str().unwrap().contains("globalThis.__senpi_import__"));
}

#[tokio::test]
async fn prepared_imports_execute_in_external_worker() {
    use maho_codemode::kernels::{js::context_manager::JavaScriptKernel, shared::subprocess_contract::KernelRunInput};
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let loader = LocalModuleLoader::new(&LocalModuleLoaderOptions {cwd:cwd.into(),local_roots:None,artifacts_dir:None}).unwrap();
    let mut kernel = JavaScriptKernel::start(cwd, "prepared-import", 4, None).await.unwrap();
    let result = kernel.run(KernelRunInput {cell_id:"import".into(),code:loader.prepare_cell("import { basename } from 'node:path'; basename('/tmp/value');"),timeout_ms:Some(5000)}, |_|{}).await.unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["valueRepr"], "\"value\"");
    kernel.close().await.unwrap();
}
