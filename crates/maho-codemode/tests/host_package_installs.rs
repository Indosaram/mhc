use maho_codemode::{
    environments::{js_environments::*, js_installer::*, revision_store::*},
    kernels::js::context_manager::JavaScriptKernel,
    tool::{
        magic_cell_host::plan_magic_cell, magic_cells::parse_magic_cell, types::EvalKernelRunInput,
    },
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

#[tokio::test]
async fn reserved_install_preserves_failure_code_and_cancels_its_operation() {
    use maho_codemode::bridges::packages_install::*;
    struct Installer(tokio::sync::mpsc::Sender<()>);
    impl PackagesInstallEnvironments for Installer {
        fn install<'a>(
            &'a self,
            _: &'a str,
            _: &'a [String],
            _: Option<f64>,
            signal: Option<maho_ext_api::AbortSignal>,
        ) -> PackagesInstallFuture<'a> {
            Box::pin(async move {
                self.0.send(()).await.expect("started");
                signal.expect("owned signal").cancelled().await;
                Err("environment_install_cancelled: stopped".into())
            })
        }
    }
    let (started, mut events) = tokio::sync::mpsc::channel(1);
    let installer = Installer(started);
    let signal = maho_ext_api::AbortSignal::default();
    let owner = signal.clone();
    let task = tokio::spawn(async move {
        run_packages_install(
            &json!({"manager":"bun","requirements":"fixture"}),
            Some(&installer),
            Some(owner),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("started")
        .expect("event");
    signal.abort();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("settled")
        .expect("join")
        .expect_err("cancelled");
    assert_eq!(error.code(), "environment_install_cancelled");
    assert_eq!(
        PackagesInstallError::Install("environment_install_failed: broken archive".into()).code(),
        "environment_install_failed"
    );
}

#[tokio::test]
async fn waiting_for_install_lock_is_cancellable_without_removing_holder_lock() {
    use maho_codemode::environments::install_lock::with_root_lock;
    let root = tempfile::tempdir().expect("root");
    let owner = maho_ext_api::AbortSignal::default();
    let held = with_root_lock(root.path(), &owner).await.expect("holder");
    let signal = maho_ext_api::AbortSignal::default();
    let pending = with_root_lock(root.path(), &signal);
    tokio::pin!(pending);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    signal.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("cancelled")
            .is_err()
    );
    drop(held);
    assert!(with_root_lock(root.path(), &owner).await.is_ok());
}

#[test]
fn js_magic_is_first_code_line_only_and_refuses_mixed_code() {
    assert!(
        parse_magic_cell("// header\n%bun add fixture")
            .expect("magic")
            .is_some()
    );
    assert!(
        parse_magic_cell("const text = `%bun add fixture`;")
            .expect("ordinary")
            .is_none()
    );
    assert!(parse_magic_cell("%npm add fixture\nprint('later')").is_err());
    assert!(parse_magic_cell("%environment elsewhere").is_err());
    assert!(
        parse_magic_cell("%pip install fixture")
            .expect("not a JS magic")
            .is_none()
    );
    assert!(parse_js_packages("--global fixture").is_err());
    assert_eq!(
        js_install_argv("bun", Path::new("root"), &["fixture".into()], None),
        ["add", "--ignore-scripts", "--cwd", "root", "fixture"]
    );
    assert_eq!(
        js_install_argv("npm", Path::new("root"), &["fixture".into()], None),
        [
            "install",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--prefix",
            "root",
            "fixture"
        ]
    );
}

#[tokio::test]
async fn project_mode_and_provisioning_policy_are_session_owned() {
    let root = tempfile::tempdir().expect("root");
    let environment = JsEnvironments::new(JsEnvironmentsOptions {
        artifacts_dir: root.path().join("artifacts"),
        cwd: root.path().into(),
        runtime: "bun".into(),
        env: Default::default(),
        settings: EnvironmentSettings {
            auto_provision: Some(false),
            ..Default::default()
        },
    });
    assert_eq!(
        environment
            .set_mode(EnvironmentMode::Project)
            .await
            .expect("project"),
        root.path()
    );
    assert!(environment.package_root().expect("project root").is_none());
    let error = environment
        .install(
            "fixture",
            &maho_ext_api::AbortSignal::default(),
            None,
            Some(JsInstallerChoice::Bun),
        )
        .await
        .expect_err("provisioning disabled");
    assert_eq!(error.code, "environment_installer_unavailable");
    assert!(!root.path().join("artifacts").exists());
}

#[tokio::test]
async fn failed_and_cancelled_revisions_do_not_replace_active_packages() {
    let root = tempfile::tempdir().expect("root");
    let signal = maho_ext_api::AbortSignal::default();
    mkdir_private(root.path()).expect("private root");
    let stage = Arc::new(stage_next_revision(root.path()).expect("stage"));
    std::fs::write(stage.dir.join("marker"), "first").expect("marker");
    publish_next_revision(stage, &signal)
        .await
        .expect("publish");
    let stage = stage_next_revision(root.path()).expect("next stage");
    std::fs::write(stage.dir.join("marker"), "failed").expect("marker");
    drop(stage);
    let stage = Arc::new(stage_next_revision(root.path()).expect("cancelled stage"));
    let abandoned = stage.dir.clone();
    signal.abort();
    assert!(publish_next_revision(stage, &signal).await.is_err());
    assert!(!abandoned.exists());
    let active = read_active_revision(root.path())
        .expect("pointer")
        .expect("active");
    assert_eq!(active.number, 1);
    assert_eq!(
        std::fs::read_to_string(active.dir.join("marker")).expect("marker"),
        "first"
    );
}

#[tokio::test]
async fn host_slot_is_fifo_and_interrupt_retains_worker_state() {
    let root = tempfile::tempdir().expect("root");
    let kernel = Arc::new(
        JavaScriptKernel::start(root.path(), "host-fifo", 2, None)
            .await
            .expect("kernel"),
    );
    let first = kernel
        .run_eval(EvalKernelRunInput {
            cell_id: "before".into(),
            code: "globalThis.hostRetained = 17".into(),
            ..Default::default()
        })
        .await
        .expect("state");
    assert_eq!(first["ok"], true);
    let (started, mut start_rx) = tokio::sync::mpsc::channel(1);
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let host_kernel = kernel.clone();
    let host = tokio::spawn(async move {
        host_kernel
            .run_eval(EvalKernelRunInput {
                cell_id: "host".into(),
                code: "throw new Error('host source reached worker')".into(),
                host_executor: Some(Box::new(move |_| {
                    Box::pin(async move {
                        started.send(()).await.expect("started");
                        wait.await.expect("release");
                        json!({"ok":true,"valueRepr":"host done"})
                    })
                })),
                ..Default::default()
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), start_rx.recv())
        .await
        .expect("host started")
        .expect("event");
    let (queued_started, mut queued_rx) = tokio::sync::mpsc::channel(1);
    let next_kernel = kernel.clone();
    let next = tokio::spawn(async move {
        next_kernel
            .run_eval(EvalKernelRunInput {
                cell_id: "after".into(),
                code: "hostRetained".into(),
                on_started: Some(Arc::new(move || {
                    queued_started.try_send(()).expect("started")
                })),
                ..Default::default()
            })
            .await
    });
    release.send(()).expect("release host");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), host)
            .await
            .expect("host settled")
            .expect("join")
            .expect("host")["ok"],
        true
    );
    tokio::time::timeout(Duration::from_secs(5), queued_rx.recv())
        .await
        .expect("next started")
        .expect("event");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), next)
            .await
            .expect("next settled")
            .expect("join")
            .expect("next")["valueRepr"],
        "17"
    );
    let (started, mut start_rx) = tokio::sync::mpsc::channel(1);
    let (cancelled, mut cancelled_rx) = tokio::sync::mpsc::channel(1);
    let pending_kernel = kernel.clone();
    let pending = tokio::spawn(async move {
        pending_kernel
            .run_eval(EvalKernelRunInput {
                cell_id: "cancel-host".into(),
                host_executor: Some(Box::new(move |context| {
                    Box::pin(async move {
                        started.send(()).await.expect("started");
                        context.signal.cancelled().await;
                        cancelled.send(()).await.expect("cancelled");
                        json!({"ok":false})
                    })
                })),
                ..Default::default()
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), start_rx.recv())
        .await
        .expect("started")
        .expect("event");
    assert!(
        kernel
            .interrupt_outcome("stop", Some("cancel-host"))
            .await
            .expect("interrupt")
            .0
    );
    tokio::time::timeout(Duration::from_secs(5), cancelled_rx.recv())
        .await
        .expect("cancelled")
        .expect("event");
    assert_eq!(
        pending.await.expect("join").expect("cancel result")["ok"],
        false
    );
    assert_eq!(
        kernel
            .run_eval(EvalKernelRunInput {
                cell_id: "retained".into(),
                code: "hostRetained".into(),
                ..Default::default()
            })
            .await
            .expect("retained")["valueRepr"],
        "17"
    );
    kernel.close().await.expect("close");
}

#[tokio::test]
async fn local_packages_accumulate_and_resolve_in_the_persistent_kernel() {
    let root = tempfile::tempdir().expect("root");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).expect("project");
    for name in ["fixture-one", "fixture-two"] {
        let source = cwd.join(name);
        std::fs::create_dir(&source).expect("source");
        std::fs::write(
            source.join("package.json"),
            json!({"name":name,"version":"1.0.0","type":"module","exports":"./index.js"})
                .to_string(),
        )
        .expect("manifest");
        std::fs::write(
            source.join("index.js"),
            format!("export default '{}';", name),
        )
        .expect("module");
    }
    let environment = Arc::new(JsEnvironments::new(JsEnvironmentsOptions {
        artifacts_dir: root.path().join("artifacts"),
        cwd: cwd.clone(),
        runtime: "bun".into(),
        env: std::env::vars().collect(),
        settings: Default::default(),
    }));
    let kernel = JavaScriptKernel::start(&cwd, "managed-installs", 2, None)
        .await
        .expect("kernel");
    for (index, name) in ["fixture-one", "fixture-two"].iter().enumerate() {
        let code = format!("%bun add ./{name}");
        let (output, mut frames) = tokio::sync::mpsc::unbounded_channel();
        let result = kernel
            .run_eval(EvalKernelRunInput {
                cell_id: format!("install-{index}"),
                code: code.clone(),
                host_executor: plan_magic_cell(&code, Some(environment.clone())),
                on_message: Some(Arc::new(move |message| {
                    output.send(message.clone()).expect("frame")
                })),
                ..Default::default()
            })
            .await
            .expect("install");
        assert_eq!(result["ok"], true, "{result}");
        let frames = std::iter::from_fn(|| frames.try_recv().ok()).collect::<Vec<Value>>();
        let ops = frames
            .iter()
            .filter_map(|frame| frame["event"]["op"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(ops, ["timeout-pause", "timeout-resume"]);
    }
    let revision = environment
        .package_root()
        .expect("package root")
        .expect("revision");
    assert_eq!(revision.file_name().expect("revision name"), "rev-2");
    let resolve = environment.clone();
    let result=kernel.run_eval(EvalKernelRunInput {cell_id:"imports".into(),code:"const a = await import('fixture-one'); const b = await import('fixture-two'); [a.default,b.default]".into(),package_root:Some(Arc::new(move ||resolve.package_root().map_err(|error|error.to_string()))),..Default::default()}).await.expect("imports");
    assert_eq!(result["ok"], true, "{result}");
    assert!(
        result["valueRepr"]
            .as_str()
            .expect("value")
            .contains("fixture-one")
    );
    assert!(
        result["valueRepr"]
            .as_str()
            .expect("value")
            .contains("fixture-two")
    );
    assert!(!cwd.join("node_modules").exists());
    kernel.close().await.expect("close");
}

#[cfg(unix)]
#[test]
fn managed_copy_rejects_unrecorded_external_links_and_strips_registry_credentials() {
    use maho_codemode::environments::{
        js_revision_files::carry_npmrc_settings, no_links::assert_built_revision_links,
    };
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).expect("link");
    assert!(assert_built_revision_links(root.path()).is_err());
    std::fs::write(root.path().join(".npmrc"),"registry=https://user:secret@example.test/path?token=secret#secret\n_authToken=secret\nstrict-ssl=true\n").expect("npmrc");
    carry_npmrc_settings(root.path()).expect("sanitize");
    let config = std::fs::read_to_string(root.path().join(".npmrc")).expect("config");
    assert_eq!(
        config,
        "registry=https://example.test/path\nstrict-ssl=true\n"
    );
}
