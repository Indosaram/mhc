use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use isolation_core::test_support::fixture;
use isolation_core::{
    is_at_or_below, IsolationBackend, IsolationContext, LoadReflink, ReflinkBackend,
};

use crate::fake::FakeRuntime;

fn linux_io(observe: Arc<dyn Fn(&[String], &Path) + Send + Sync>) -> FakeRuntime {
    FakeRuntime {
        device: Arc::new(move |path: &Path| {
            observe(&[], path);
            Ok(1)
        }),
        run: Arc::new(move |argv: &[String]| {
            let path = argv.get(4).map(PathBuf::from).unwrap_or_default();
            observe(argv, &path);
            Ok(crate::fake::ok())
        }),
        ..FakeRuntime::linux()
    }
}

#[test]
fn reflink_probing_writes_only_inside_the_supplied_context_base_directory() {
    let f = fixture();
    let seen: Arc<Mutex<(Vec<PathBuf>, Vec<PathBuf>)>> =
        Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let sink = Arc::clone(&seen);
    let io = linux_io(Arc::new(move |argv, path| {
        let mut guard = sink.lock().unwrap_or_else(|poison| poison.into_inner());
        if argv.is_empty() {
            guard.0.push(path.to_path_buf());
        } else {
            guard.1.push(path.to_path_buf());
        }
    }));
    let ctx = IsolationContext {
        id: "probe".to_string(),
        base_dir: f.root.join("wt/t1"),
        cross_device: false,
        max_copy_bytes: None,
    };
    let load: LoadReflink = Arc::new(|| None);
    let backend = ReflinkBackend::new(Arc::new(io), load);
    assert!(backend
        .probe(&f.repo_root, Some(&ctx))
        .expect("probe")
        .available);
    let guard = seen.lock().unwrap_or_else(|poison| poison.into_inner());
    let violations: Vec<PathBuf> = guard
        .0
        .iter()
        .chain(guard.1.iter())
        .filter(|path| {
            **path != f.repo_root && **path != ctx.base_dir && !is_at_or_below(path, &ctx.base_dir)
        })
        .cloned()
        .collect();
    assert!(violations.is_empty());
}

#[test]
fn reflink_probing_without_a_context_stays_read_only_instead_of_writing_into_the_source() {
    let f = fixture();
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let io = linux_io(Arc::new(move |argv, _path| {
        if !argv.is_empty() {
            sink.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(argv.to_vec());
        }
    }));
    let load: LoadReflink = Arc::new(|| None);
    let backend = ReflinkBackend::new(Arc::new(io), load);
    let probe = backend.probe(&f.repo_root, None).expect("probe");
    assert!(!probe.available);
    assert!(calls
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .is_empty());
}
