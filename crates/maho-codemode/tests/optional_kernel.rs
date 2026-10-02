use maho_codemode::kernels::jl::kernel::{JuliaKernel, resolve_julia_runner_path};
use maho_codemode::kernels::rb::kernel::{RubyKernel, resolve_ruby_runner_path};
use maho_codemode::kernels::shared::subprocess_contract::SubprocessKernelOptions;
use maho_codemode::bridge::protocol::BridgeConnectionConfig;

fn missing_options() -> SubprocessKernelOptions {
    SubprocessKernelOptions {
        command: "/absent/optional-interpreter".into(), args: vec![], cwd: env!("CARGO_MANIFEST_DIR").into(),
        env: None, session_env: None, session_id: "optional-test".into(),
        connection: BridgeConnectionConfig { port: 1, token: "test".into(), local_roots: None, artifacts_dir: None, parallel_pool_width: None },
    }
}

#[test]
fn optional_runner_assets_exist() {
    assert!(resolve_ruby_runner_path().is_file());
    assert!(resolve_julia_runner_path().is_file());
}

#[test]
fn julia_disables_startup_history_color_and_jit() {
    let args = JuliaKernel::arguments();
    assert_eq!(&args[..5], ["--startup-file=no", "--history-file=no", "--color=no", "--compile=min", "--optimize=0"]);
    assert_eq!(args.last().unwrap(), &resolve_julia_runner_path().to_string_lossy());
}

#[tokio::test]
async fn missing_ruby_reports_capability_failure() { assert!(RubyKernel::start(missing_options()).await.is_err()); }

#[tokio::test]
async fn missing_julia_reports_capability_failure() { assert!(JuliaKernel::start(missing_options()).await.is_err()); }

#[tokio::test]
async fn real_ruby_persistent_cells() {
    let mut options = missing_options();
    options.command = "ruby".into();
    let mut env = std::env::vars().collect::<std::collections::HashMap<String, String>>();
    env.insert("RUBYLIB".into(), concat!(env!("CARGO_MANIFEST_DIR"), "/tests/runtime/base64-0.3.0/lib").into());
    options.env = Some(env);
    let kernel = RubyKernel::start(options).await.unwrap();
    for (code, expected) in [("x = 7; x", "7"), ("x + 1", "8")] {
        let result = kernel.run(maho_codemode::kernels::shared::subprocess_contract::KernelRunInput { cell_id: "ruby".into(), code: code.into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["valueRepr"], expected);
    }
    kernel.close().await.unwrap();
}
