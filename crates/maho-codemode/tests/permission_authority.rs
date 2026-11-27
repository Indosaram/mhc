use maho_codemode::{
    bridge::permission_check::{permission_check, register_permission_check},
    kernels::{
        js::{
            interrupt_bounds::WorkerRetirement,
            permission_host::{
                KernelPermissionHost, KernelPermissionHostOptions, revoke_session_kernels,
            },
            permission_worker_frames::{worker_error_frame, worker_messageerror_frame},
        },
        shared::{
            cell_authority_registry::InvocationSessionSnapshot,
            invocation_authority::invocation_session,
            path_identity::{CanonicalPath, PATH_IDENTITY_VERSION},
            session_spawn_attestation::{AttestKernelSpawn, SessionSpawnAttestationHandle},
        },
    },
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct DummyAttestation;
impl SessionSpawnAttestationHandle for DummyAttestation {
    fn release(&self) {}
}

#[tokio::test]
async fn permission_check_registry_validates_payloads_and_drops_on_unregistration() {
    let reg = register_permission_check(
        "tok-check",
        Arc::new(|body| {
            Box::pin(async move {
                Ok(json!({
                    "allowed": body["tool"] == "read",
                }))
            })
        }),
    );

    let ok = permission_check(&json!({
        "token": "tok-check",
        "nonce": "n-1",
        "call": { "tool": "read", "input": { "path": "/tmp/a" } },
    }))
    .await
    .expect("valid read permission check");
    assert_eq!(ok["decision"]["allowed"], true);

    let bad_tool = permission_check(&json!({
        "token": "tok-check",
        "nonce": "n-2",
        "call": { "tool": "eval", "input": { "path": "/tmp/a" } },
    }))
    .await;
    assert!(bad_tool.is_err());

    let missing_cmd = permission_check(&json!({
        "token": "tok-check",
        "nonce": "n-3",
        "call": { "tool": "bash", "input": {} },
    }))
    .await;
    assert!(missing_cmd.is_err());

    drop(reg);
    let after_drop = permission_check(&json!({
        "token": "tok-check",
        "nonce": "n-4",
        "call": { "tool": "read", "input": { "path": "/tmp/a" } },
    }))
    .await;
    assert!(after_drop.is_err());
}

#[tokio::test]
async fn permission_host_cross_session_authority_and_session_revocation() {
    let retires = Arc::new(AtomicUsize::new(0));
    let retires_cb = Arc::clone(&retires);
    let host = KernelPermissionHost::new(
        KernelPermissionHostOptions {
            session_id: "sess-owner".into(),
            cwd: "/work".into(),
            read_state: Some(Arc::new(|| (3, false, Vec::new()))),
            resolve_session: None,
        },
        Arc::new(move || {
            let retires_cb = Arc::clone(&retires_cb);
            Box::pin(async move {
                retires_cb.fetch_add(1, Ordering::SeqCst);
                WorkerRetirement::Terminated
            })
        }),
    );

    assert!(!host.blocked());
    let run_tok = host.begin_run("cell-1").expect("run token");
    assert!(run_tok.is_live());
    assert!(!run_tok.is_enforced());

    // Cross-session refusal when owner or invoker enforces deny rules.
    let err = invocation_session(
        "sess-a",
        &InvocationSessionSnapshot {
            cwd: "/a".into(),
            epoch: 1,
            active: true,
            rules: vec![],
        },
        Some("sess-b"),
        None,
    )
    .unwrap_err();
    assert_eq!(err.code, "cell_permission_denied");

    revoke_session_kernels("sess-owner").await;
    assert!(!run_tok.is_live());
    assert!(host.blocked());
    assert!(host.assert_session_live().is_err());
    assert_eq!(retires.load(Ordering::SeqCst), 1);

    host.dispose();
}

#[test]
fn permission_worker_frames_and_shared_contract_types() {
    let err_frame = worker_error_frame("boom", Some("cell.js"), Some(4), Some(2));
    assert_eq!(err_frame.frame_type, "error");
    assert_eq!(err_frame.lineno, 4);
    let msg_err = worker_messageerror_frame("DataCloneError", "bad port");
    assert_eq!(msg_err["type"], "messageerror");

    assert_eq!(PATH_IDENTITY_VERSION, 1);
    let canonical = CanonicalPath {
        path: "/tmp/file".into(),
        fold_case: false,
    };
    assert_eq!(canonical.path, "/tmp/file");

    let attest: AttestKernelSpawn = Arc::new(|_pid| Box::new(DummyAttestation));
    attest(123).release();
}

#[tokio::test]
async fn permission_host_invocation_and_settle_release_tokens() {
    let host = KernelPermissionHost::new(
        KernelPermissionHostOptions {
            session_id: "sess-inv".into(),
            cwd: "/work".into(),
            read_state: Some(Arc::new(|| (2, true, Vec::new()))),
            resolve_session: None,
        },
        Arc::new(|| Box::pin(async { WorkerRetirement::Terminated })),
    );

    let tok = host
        .begin_invocation("call-1", None)
        .expect("same session invocation");
    assert!(tok.is_live());
    assert!(tok.is_enforced());
    host.end_invocation("call-1");
    assert!(!tok.is_live());

    let run_tok = host.begin_run("cell-settle").expect("run");
    host.settle_run("cell-settle");
    assert!(!run_tok.is_live());
    host.dispose();
}

#[tokio::test]
async fn session_revocation_kills_multiple_registered_kernels_simultaneously() {
    let retires = Arc::new(AtomicUsize::new(0));
    let mk = || {
        let retires = Arc::clone(&retires);
        KernelPermissionHost::new(
            KernelPermissionHostOptions {
                session_id: "shared-sess".into(),
                cwd: "/work".into(),
                read_state: None,
                resolve_session: None,
            },
            Arc::new(move || {
                let retires = Arc::clone(&retires);
                Box::pin(async move {
                    retires.fetch_add(1, Ordering::SeqCst);
                    WorkerRetirement::Terminated
                })
            }),
        )
    };
    let k1 = mk();
    let k2 = mk();
    let t1 = k1.begin_run("c1").expect("t1");
    let t2 = k2.begin_run("c2").expect("t2");
    revoke_session_kernels("shared-sess").await;
    assert!(!t1.is_live());
    assert!(!t2.is_live());
    assert_eq!(retires.load(Ordering::SeqCst), 2);
    k1.dispose();
    k2.dispose();
}
