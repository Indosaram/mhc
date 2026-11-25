use super::harness::{Fixture, INSIDE_TEXT, READ_LIMIT, SECRET_TEXT, write_oversized_file};
use maho_machine_ops::{
    EditRequest, EntryType, GlobRequest, GrepRequest, ListRequest, MachineOpError, OpContext,
    OpErrorCode, PolicyDecision, PolicyHooks, ReadRequest, StatRequest, WriteRequest,
    op_error_code,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

fn assert_code(err: MachineOpError, expected: OpErrorCode) {
    assert_eq!(
        op_error_code(&err),
        expected,
        "expected {expected}, got {err}"
    );
}

#[tokio::test]
async fn files_read_inside_root() {
    let f = Fixture::create(true).await;
    let res = f
        .ops
        .files()
        .read(
            ReadRequest {
                path: "inside.txt".to_owned(),
                offset: None,
                limit_bytes: None,
            },
            &f.ctx(),
        )
        .await
        .expect("read inside");
    assert_eq!(String::from_utf8_lossy(&res.bytes), INSIDE_TEXT);
    assert_eq!(res.size, INSIDE_TEXT.len() as u64);
}

#[tokio::test]
async fn files_read_outside_root_denied() {
    let f = Fixture::create(true).await;
    let err = f
        .ops
        .files()
        .read(
            ReadRequest {
                path: f.outside.join("secret.txt").to_string_lossy().into_owned(),
                offset: None,
                limit_bytes: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("outside denied");
    assert_code(err, OpErrorCode::PolicyDenied);
}

#[tokio::test]
async fn files_read_escape_symlink_denied() {
    let f = Fixture::create(true).await;
    let err = f
        .ops
        .files()
        .read(
            ReadRequest {
                path: "escape/secret.txt".to_owned(),
                offset: None,
                limit_bytes: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("symlink escape denied");
    assert_code(err, OpErrorCode::PolicyDenied);
}

#[tokio::test]
async fn files_read_missing_not_found() {
    let f = Fixture::create(true).await;
    let err = f
        .ops
        .files()
        .read(
            ReadRequest {
                path: "missing.txt".to_owned(),
                offset: None,
                limit_bytes: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("missing");
    assert_code(err, OpErrorCode::NotFound);
}

#[tokio::test]
async fn files_read_above_limit_exceeded() {
    let f = Fixture::create(true).await;
    write_oversized_file(&f.root.join("big.bin")).await;
    let err = f
        .ops
        .files()
        .read(
            ReadRequest {
                path: "big.bin".to_owned(),
                offset: None,
                limit_bytes: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("limit exceeded");
    assert_code(err, OpErrorCode::LimitExceeded);
}

#[tokio::test]
async fn files_write_inside_and_denials_and_ranges() {
    let f = Fixture::create(true).await;
    let res = f
        .ops
        .files()
        .write(
            WriteRequest {
                path: "new.txt".to_owned(),
                bytes: b"written".to_vec(),
                create_parent_dirs: false,
                offset: None,
                total: None,
            },
            &f.ctx(),
        )
        .await
        .expect("write inside");
    assert_eq!(res.written_bytes, 7);
    assert_eq!(
        tokio::fs::read_to_string(f.root.join("new.txt"))
            .await
            .expect("read back"),
        "written"
    );

    let err = f
        .ops
        .files()
        .write(
            WriteRequest {
                path: f.outside.join("secret.txt").to_string_lossy().into_owned(),
                bytes: b"x".to_vec(),
                create_parent_dirs: false,
                offset: None,
                total: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("outside denied");
    assert_code(err, OpErrorCode::PolicyDenied);
    assert_eq!(
        tokio::fs::read_to_string(f.outside.join("secret.txt"))
            .await
            .expect("secret untouched"),
        SECRET_TEXT
    );

    let err = f
        .ops
        .files()
        .write(
            WriteRequest {
                path: "escape/planted.txt".to_owned(),
                bytes: b"x".to_vec(),
                create_parent_dirs: false,
                offset: None,
                total: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("escape denied");
    assert_code(err, OpErrorCode::PolicyDenied);
    assert!(!f.outside.join("planted.txt").exists());

    let err = f
        .ops
        .files()
        .write(
            WriteRequest {
                path: "nope/x.txt".to_owned(),
                bytes: b"x".to_vec(),
                create_parent_dirs: false,
                offset: None,
                total: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("missing parent");
    assert_code(err, OpErrorCode::NotFound);

    f.ops
        .files()
        .write(
            WriteRequest {
                path: "nope/x.txt".to_owned(),
                bytes: b"x".to_vec(),
                create_parent_dirs: true,
                offset: None,
                total: None,
            },
            &f.ctx(),
        )
        .await
        .expect("createParentDirs");
    assert_eq!(
        tokio::fs::read_to_string(f.root.join("nope/x.txt"))
            .await
            .expect("read nope/x.txt"),
        "x"
    );

    // Oversized write + out-of-order range assembly.
    let big = vec![b'b'; READ_LIMIT + 1];
    f.ops
        .files()
        .write(
            WriteRequest {
                path: "big.bin".to_owned(),
                bytes: big,
                create_parent_dirs: false,
                offset: None,
                total: None,
            },
            &f.ctx(),
        )
        .await
        .expect("write big.bin");
    assert_eq!(
        tokio::fs::metadata(f.root.join("big.bin"))
            .await
            .expect("stat big.bin")
            .len(),
        (READ_LIMIT + 1) as u64
    );

    f.ops
        .files()
        .write(
            WriteRequest {
                path: "ranged.txt".to_owned(),
                bytes: b"world".to_vec(),
                create_parent_dirs: false,
                offset: Some(6),
                total: Some(11),
            },
            &f.ctx(),
        )
        .await
        .expect("range 2");
    f.ops
        .files()
        .write(
            WriteRequest {
                path: "ranged.txt".to_owned(),
                bytes: b"hello ".to_vec(),
                create_parent_dirs: false,
                offset: Some(0),
                total: Some(11),
            },
            &f.ctx(),
        )
        .await
        .expect("range 1");
    assert_eq!(
        tokio::fs::read_to_string(f.root.join("ranged.txt"))
            .await
            .expect("read ranged"),
        "hello world"
    );
}

#[tokio::test]
async fn files_edit_cases() {
    let f = Fixture::create(true).await;
    let res = f
        .ops
        .files()
        .edit(
            EditRequest {
                path: "inside.txt".to_owned(),
                exact: "l".to_owned(),
                replacement: "L".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect("edit inside");
    assert_eq!(res.replacements, INSIDE_TEXT.matches('l').count());
    assert_eq!(res.content, INSIDE_TEXT.replace('l', "L"));

    let err = f
        .ops
        .files()
        .edit(
            EditRequest {
                path: f.outside.join("secret.txt").to_string_lossy().into_owned(),
                exact: "secret".to_owned(),
                replacement: "x".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect_err("outside denied");
    assert_code(err, OpErrorCode::PolicyDenied);

    let err = f
        .ops
        .files()
        .edit(
            EditRequest {
                path: "escape/secret.txt".to_owned(),
                exact: "secret".to_owned(),
                replacement: "x".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect_err("escape denied");
    assert_code(err, OpErrorCode::PolicyDenied);

    let err = f
        .ops
        .files()
        .edit(
            EditRequest {
                path: "missing.txt".to_owned(),
                exact: "a".to_owned(),
                replacement: "b".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect_err("missing");
    assert_code(err, OpErrorCode::NotFound);

    let err = f
        .ops
        .files()
        .edit(
            EditRequest {
                path: "inside.txt".to_owned(),
                exact: "".to_owned(),
                replacement: "x".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect_err("empty exact");
    assert_code(err, OpErrorCode::InvalidPattern);

    let err = f
        .ops
        .files()
        .edit(
            EditRequest {
                path: "inside.txt".to_owned(),
                exact: "absent".to_owned(),
                replacement: "x".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect_err("absent exact");
    assert_code(err, OpErrorCode::NotFound);
}

#[tokio::test]
async fn files_glob_cases() {
    let f = Fixture::create(true).await;
    let res = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: "*.ts".to_owned(),
                cwd: Some("dir".to_owned()),
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect("glob inside dir");
    assert_eq!(res.entries.len(), 1);
    assert_eq!(res.entries[0].path, f.root.join("dir/a.ts"));

    let res_abs = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: f.root.join("dir/*.ts").to_string_lossy().into_owned(),
                cwd: None,
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect("glob absolute inside root");
    assert_eq!(res_abs.entries[0].path, f.root.join("dir/a.ts"));

    let res_rel = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: "../dir/*.ts".to_owned(),
                cwd: Some("dir".to_owned()),
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect("glob .. staying inside root");
    assert_eq!(res_rel.entries[0].path, f.root.join("dir/a.ts"));

    let err = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: f.outside.join("*").to_string_lossy().into_owned(),
                cwd: None,
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("outside glob");
    assert_code(err, OpErrorCode::PolicyDenied);

    let err = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: "../*".to_owned(),
                cwd: None,
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("dotdot escaping root");
    assert_code(err, OpErrorCode::PolicyDenied);

    let err = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: "dir/*/../a.ts".to_owned(),
                cwd: None,
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("dotdot after wildcard");
    assert_code(err, OpErrorCode::PolicyDenied);

    let err = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: f.root.join("escape/*").to_string_lossy().into_owned(),
                cwd: None,
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("glob through escape");
    assert_code(err, OpErrorCode::PolicyDenied);

    let err = f
        .ops
        .files()
        .glob(
            GlobRequest {
                pattern: "*".to_owned(),
                cwd: Some("missing-dir".to_owned()),
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("missing dir");
    assert_code(err, OpErrorCode::NotFound);
}

#[tokio::test]
async fn files_grep_stat_list_cases() {
    let f = Fixture::create(true).await;

    let grep_res = f
        .ops
        .files()
        .grep(
            GrepRequest {
                path: ".".to_owned(),
                pattern: "second".to_owned(),
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect("grep");
    assert_eq!(grep_res.matches.len(), 1);
    assert_eq!(grep_res.matches[0].line, 2);
    assert_eq!(grep_res.matches[0].text, "second line");

    let err = f
        .ops
        .files()
        .grep(
            GrepRequest {
                path: "escape".to_owned(),
                pattern: "secret".to_owned(),
                limit: None,
            },
            &f.ctx(),
        )
        .await
        .expect_err("grep escape");
    assert_code(err, OpErrorCode::PolicyDenied);

    let stat_res = f
        .ops
        .files()
        .stat(
            StatRequest {
                path: "inside.txt".to_owned(),
                access: None,
            },
            &f.ctx(),
        )
        .await
        .expect("stat file");
    assert_eq!(stat_res.entry_type, EntryType::File);
    assert_eq!(stat_res.size, INSIDE_TEXT.len() as u64);

    let list_res = f
        .ops
        .files()
        .list(
            ListRequest {
                path: ".".to_owned(),
            },
            &f.ctx(),
        )
        .await
        .expect("list root");
    let map: BTreeMap<String, EntryType> = list_res
        .entries
        .into_iter()
        .map(|e| (e.name, e.entry_type))
        .collect();
    assert_eq!(map.get("dir"), Some(&EntryType::Directory));
    assert_eq!(map.get("escape"), Some(&EntryType::Symlink));
    assert_eq!(map.get("inside.txt"), Some(&EntryType::File));
}

#[tokio::test]
async fn mutation_queue_serializes_same_path_and_overlaps_different_paths() {
    let f = Fixture::create(true).await;
    tokio::fs::write(f.root.join("shared.txt"), "zero")
        .await
        .expect("write shared");

    let first_entered = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let order = Arc::new(Mutex::new(Vec::<String>::new()));

    let before_op = {
        let first_entered = Arc::clone(&first_entered);
        let release_first = Arc::clone(&release_first);
        let order = Arc::clone(&order);
        Arc::new(
            move |op: maho_machine_ops::OpName, _args: &serde_json::Value, _ctx: &OpContext| {
                let first_entered = Arc::clone(&first_entered);
                let release_first = Arc::clone(&release_first);
                let order = Arc::clone(&order);
                Box::pin(async move {
                    let label = op.as_str().to_owned();
                    order.lock().expect("lock").push(label.clone());
                    if label == "files.write" {
                        first_entered.notify_one();
                        release_first.notified().await;
                    }
                    Ok(PolicyDecision::Allow)
                })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<Output = Result<PolicyDecision, MachineOpError>>
                                + Send,
                        >,
                    >
            },
        )
    };

    let ctx1 = f.ctx_with(
        PolicyHooks {
            before_op: Some(before_op.clone()),
            ..Default::default()
        },
        None,
    );
    let ctx2 = f.ctx_with(
        PolicyHooks {
            before_op: Some(before_op),
            ..Default::default()
        },
        None,
    );

    let entered_fut = first_entered.notified();
    let ops1 = Arc::clone(&f.ops);
    let first = tokio::spawn(async move {
        ops1.files()
            .write(
                WriteRequest {
                    path: "shared.txt".to_owned(),
                    bytes: b"first".to_vec(),
                    create_parent_dirs: false,
                    offset: None,
                    total: None,
                },
                &ctx1,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered_fut)
        .await
        .expect("first write entered gate");

    let ops2 = Arc::clone(&f.ops);
    let second = tokio::spawn(async move {
        ops2.files()
            .edit(
                EditRequest {
                    path: "./dir/../shared.txt".to_owned(),
                    exact: "first".to_owned(),
                    replacement: "second".to_owned(),
                },
                &ctx2,
            )
            .await
    });
    release_first.notify_one();
    first.await.expect("join1").expect("first ok");
    let edited = second.await.expect("join2").expect("second ok");
    assert_eq!(edited.replacements, 1);
    assert_eq!(
        *order.lock().expect("lock"),
        vec!["files.write", "files.edit"]
    );
    assert_eq!(
        tokio::fs::read_to_string(f.root.join("shared.txt"))
            .await
            .expect("read shared"),
        "second"
    );

    // Overlap check on distinct files:
    let a_entered = Arc::new(Notify::new());
    let b_entered = Arc::new(Notify::new());
    let release_a = Arc::new(Notify::new());
    let overlap_hook = {
        let a_entered = Arc::clone(&a_entered);
        let b_entered = Arc::clone(&b_entered);
        let release_a = Arc::clone(&release_a);
        Arc::new(
            move |_op: maho_machine_ops::OpName, args: &serde_json::Value, _ctx: &OpContext| {
                let a_entered = Arc::clone(&a_entered);
                let b_entered = Arc::clone(&b_entered);
                let release_a = Arc::clone(&release_a);
                let path = args["path"].as_str().unwrap_or_default().to_owned();
                Box::pin(async move {
                    if path == "a.txt" {
                        a_entered.notify_one();
                        release_a.notified().await;
                    } else {
                        b_entered.notify_one();
                    }
                    Ok(PolicyDecision::Allow)
                })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<Output = Result<PolicyDecision, MachineOpError>>
                                + Send,
                        >,
                    >
            },
        )
    };
    let ctx_a = f.ctx_with(
        PolicyHooks {
            before_op: Some(overlap_hook.clone()),
            ..Default::default()
        },
        None,
    );
    let ctx_b = f.ctx_with(
        PolicyHooks {
            before_op: Some(overlap_hook),
            ..Default::default()
        },
        None,
    );
    let a_notified = a_entered.notified();
    let b_notified = b_entered.notified();
    let ops_a = Arc::clone(&f.ops);
    let a = tokio::spawn(async move {
        ops_a
            .files()
            .write(
                WriteRequest {
                    path: "a.txt".to_owned(),
                    bytes: b"a".to_vec(),
                    create_parent_dirs: false,
                    offset: None,
                    total: None,
                },
                &ctx_a,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), a_notified)
        .await
        .expect("a entered");
    let ops_b = Arc::clone(&f.ops);
    let b = tokio::spawn(async move {
        ops_b
            .files()
            .write(
                WriteRequest {
                    path: "b.txt".to_owned(),
                    bytes: b"b".to_vec(),
                    create_parent_dirs: false,
                    offset: None,
                    total: None,
                },
                &ctx_b,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), b_notified)
        .await
        .expect("b entered while a was held");
    b.await.expect("join b").expect("b ok");
    release_a.notify_one();
    a.await.expect("join a").expect("a ok");
}
