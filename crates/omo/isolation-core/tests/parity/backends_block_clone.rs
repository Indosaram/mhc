use std::path::Path;
use std::sync::{Arc, Mutex};

use isolation_core::test_support::fixture;
use isolation_core::{
    create_windows_clone_api, duplicate_extents, BlockCloneBackend, IsolationBackend,
    IsolationContext, NativeLoadError, WindowsCloneApi, WindowsSymbols,
};

use crate::fake::{record, FakeRuntime};

const FSCTL_DUPLICATE_EXTENTS_TO_FILE: u32 = 0x00098344;

struct FakeSymbols {
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
    fail_ioctl: bool,
    errno: u32,
}

impl WindowsSymbols for FakeSymbols {
    fn create_file_w(
        &self,
        path: &[u8],
        access: u32,
        share: u32,
        disposition: u32,
        flags: u32,
    ) -> i64 {
        let units: Vec<u16> = path
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!([
                "CreateFileW",
                String::from_utf16_lossy(&units),
                access,
                share,
                serde_json::Value::Null,
                disposition,
                flags,
                serde_json::Value::Null
            ]));
        if access == 0x80000000 {
            1
        } else {
            2
        }
    }

    fn set_file_pointer_ex(&self, handle: i64, offset: u64) -> i32 {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!([
                "SetFilePointerEx",
                handle,
                offset,
                serde_json::Value::Null,
                0
            ]));
        1
    }

    fn set_end_of_file(&self, handle: i64) -> i32 {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!(["SetEndOfFile", handle]));
        1
    }

    fn device_io_control(&self, handle: i64, code: u32, input: &[u8], returned: &mut [u8]) -> i32 {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!([
                "DeviceIoControl",
                handle,
                code,
                input,
                input.len(),
                serde_json::Value::Null,
                0,
                returned.len(),
                serde_json::Value::Null
            ]));
        if self.fail_ioctl {
            0
        } else {
            1
        }
    }

    fn close_handle(&self, handle: i64) -> i32 {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!(["CloseHandle", handle]));
        1
    }

    fn get_last_error(&self) -> u32 {
        self.errno
    }

    fn get_disk_free_space_w(&self, _root: &[u8], sectors: &mut [u8], bytes: &mut [u8]) -> i32 {
        sectors.copy_from_slice(&8u32.to_le_bytes());
        bytes.copy_from_slice(&512u32.to_le_bytes());
        1
    }
}

fn native() -> (Arc<Mutex<Vec<serde_json::Value>>>, Arc<dyn WindowsSymbols>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let symbols: Arc<dyn WindowsSymbols> = Arc::new(FakeSymbols {
        calls: Arc::clone(&calls),
        fail_ioctl: false,
        errno: 50,
    });
    (calls, symbols)
}

#[test]
fn native_refs_binding_uses_the_actual_fsctl_opcode_handle_structure_and_eof_calls() {
    let (calls, symbols) = native();
    let api = create_windows_clone_api(symbols);
    duplicate_extents(api.as_ref(), "C:\\source", "C:\\target", 8192).expect("duplicate");
    let mut data = vec![0u8; 32];
    data[0..8].copy_from_slice(&1u64.to_le_bytes());
    data[24..32].copy_from_slice(&8192u64.to_le_bytes());
    assert_eq!(
        *calls.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![
            serde_json::json!(["CreateFileW", "\\\\?\\C:\\source\0", 0x80000000u32, 7, null, 3, 0x80u32, null]),
            serde_json::json!(["CreateFileW", "\\\\?\\C:\\target\0", 0x40000000u32, 7, null, 1, 0x80u32, null]),
            serde_json::json!(["SetFilePointerEx", 2, 8192u64, null, 0]),
            serde_json::json!(["SetEndOfFile", 2]),
            serde_json::json!(["DeviceIoControl", 2, FSCTL_DUPLICATE_EXTENTS_TO_FILE, data, 32, null, 0, 4, null]),
            serde_json::json!(["SetFilePointerEx", 2, 8192u64, null, 0]),
            serde_json::json!(["SetEndOfFile", 2]),
            serde_json::json!(["CloseHandle", 2]),
            serde_json::json!(["CloseHandle", 1]),
        ]
    );
}

#[test]
fn native_refs_errors_preserve_classification_and_close_both_handles() {
    for errno in [50u32, 5u32] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let symbols: Arc<dyn WindowsSymbols> = Arc::new(FakeSymbols {
            calls: Arc::clone(&calls),
            fail_ioctl: true,
            errno,
        });
        let api = create_windows_clone_api(symbols);
        let error = duplicate_extents(api.as_ref(), "C:\\source", "C:\\target", 8192)
            .expect_err("must fail");
        assert_eq!(error.is_unavailable(), errno == 50);
        let recorded = calls.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
        let tail = &recorded[recorded.len() - 2..];
        assert_eq!(
            tail,
            &[
                serde_json::json!(["CloseHandle", 2]),
                serde_json::json!(["CloseHandle", 1])
            ]
        );
    }
}

#[test]
fn refs_tiny_files_and_unaligned_tails_use_plain_copy_without_changing_the_source() {
    let f = fixture();
    for size in [0u64, 5, 8195] {
        let source = f.repo_root.join(format!("source-{size}"));
        let destination = f.root.join(format!("target-{size}"));
        let content = vec![42u8; size as usize];
        std::fs::write(&source, &content).expect("source");
        let cloned = Arc::new(Mutex::new(Vec::new()));
        let allocations = Arc::new(Mutex::new(Vec::new()));
        let cloned_sink = Arc::clone(&cloned);
        let allocations_sink = Arc::clone(&allocations);
        let api = TestCloneApi {
            cloned: cloned_sink,
            allocations: allocations_sink,
        };
        if size >= 4096 {
            std::fs::write(&destination, &content[..8192.min(content.len())]).expect("target");
        }
        duplicate_extents(&api, &source.to_string_lossy(), &destination.to_string_lossy(), size)
            .expect("duplicate");
        assert_eq!(
            *cloned.lock().unwrap_or_else(|poison| poison.into_inner()),
            if size >= 4096 { vec![8192] } else { Vec::new() }
        );
        assert_eq!(
            *allocations.lock().unwrap_or_else(|poison| poison.into_inner()),
            if size >= 4096 { vec![12288, 8195] } else { Vec::new() }
        );
        assert_eq!(std::fs::read(&destination).expect("target"), content);
        assert_eq!(std::fs::read(&source).expect("source"), content);
    }
}

struct TestCloneApi {
    cloned: Arc<Mutex<Vec<u64>>>,
    allocations: Arc<Mutex<Vec<u64>>>,
}

impl WindowsCloneApi for TestCloneApi {
    fn open(&self, _path: &str, write: bool) -> isolation_core::IsolationResult<u64> {
        Ok(if write { 2 } else { 1 })
    }

    fn resize(&self, _handle: u64, size: u64) -> isolation_core::IsolationResult<()> {
        self.allocations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(size);
        Ok(())
    }

    fn duplicate(&self, _destination: u64, _source: u64, bytes: u64) -> isolation_core::IsolationResult<()> {
        self.cloned
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(bytes);
        Ok(())
    }

    fn close(&self, _handle: u64) -> isolation_core::IsolationResult<()> {
        Ok(())
    }

    fn cluster_size(&self, _path: &str) -> isolation_core::IsolationResult<u64> {
        Ok(4096)
    }
}

#[test]
fn refs_probe_loads_ffi_only_on_a_matching_filesystem_and_rejects_cross_volume_targets() {
    let f = fixture();
    let calls = crate::fake::calls();
    let sink = Arc::clone(&calls);
    let repo_root = f.repo_root.clone();
    let runtime = FakeRuntime {
        platform: "win32".to_string(),
        which: Arc::new(|_| true),
        device: Arc::new(move |path: &Path| Ok(if path == repo_root { 1 } else { 2 })),
        run: Arc::new(move |argv: &[String]| {
            record(&sink, argv);
            Ok(crate::fake::result(0, "File System Name : ReFS", ""))
        }),
        ..FakeRuntime::linux()
    };
    let loaded = Arc::new(Mutex::new(0usize));
    let load_sink = Arc::clone(&loaded);
    let load: isolation_core::LoadWindowsApi = Arc::new(move || {
        *load_sink.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
        let (_, symbols) = native();
        Ok(create_windows_clone_api(symbols))
    });
    let backend = BlockCloneBackend::new(Arc::new(runtime), load);
    assert!(backend.probe(Path::new("R:\\repo"), None).expect("probe").available);
    assert_eq!(crate::fake::recorded(&calls).len(), 1);
    assert_eq!(crate::fake::recorded(&calls)[0].len(), 4);
    assert_eq!(*loaded.lock().unwrap_or_else(|poison| poison.into_inner()), 1);
    let error = backend
        .start(
            &f.repo_root,
            &f.root.join("m"),
            &IsolationContext {
                id: "cross".to_string(),
                base_dir: f.root.clone(),
                cross_device: false,
                max_copy_bytes: None,
            },
        )
        .expect_err("must fail");
    assert!(error.is_unavailable());
}

#[test]
fn refs_probe_treats_absent_native_ffi_as_unavailable_but_propagates_other_loader_errors() {
    let runtime = FakeRuntime {
        platform: "win32".to_string(),
        which: Arc::new(|_| true),
        run: Arc::new(|_| Ok(crate::fake::result(0, "ReFS", ""))),
        ..FakeRuntime::linux()
    };
    let missing: isolation_core::LoadWindowsApi = Arc::new(|| {
        Err(NativeLoadError {
            code: Some("ERR_UNSUPPORTED_ESM_URL_SCHEME".to_string()),
            message: "unsupported bun: scheme".to_string(),
        })
    });
    assert!(!BlockCloneBackend::new(Arc::new(runtime.clone()), missing)
        .probe(Path::new("R:\\repo"), None)
        .expect("probe")
        .available);
    let broken: isolation_core::LoadWindowsApi = Arc::new(|| {
        Err(NativeLoadError {
            code: None,
            message: "loader failure".to_string(),
        })
    });
    let error = BlockCloneBackend::new(Arc::new(runtime), broken)
        .probe(Path::new("R:\\repo"), None)
        .expect_err("must fail");
    assert!(error.to_string().contains("loader failure"));
}

#[test]
fn refs_probe_rejects_a_different_target_volume_before_loading_native_symbols() {
    let f = fixture();
    let loads = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&loads);
    let repo_root = f.repo_root.clone();
    let runtime = FakeRuntime {
        platform: "win32".to_string(),
        which: Arc::new(|_| true),
        device: Arc::new(move |path: &Path| Ok(if path == repo_root { 1 } else { 2 })),
        run: Arc::new(|_| Ok(crate::fake::result(0, "ReFS", ""))),
        ..FakeRuntime::linux()
    };
    let load: isolation_core::LoadWindowsApi = Arc::new(move || {
        *sink.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
        let (_, symbols) = native();
        Ok(create_windows_clone_api(symbols))
    });
    let backend = BlockCloneBackend::new(Arc::new(runtime), load);
    let context = IsolationContext {
        id: "cross".to_string(),
        base_dir: f.home_dir.clone(),
        cross_device: false,
        max_copy_bytes: None,
    };
    assert!(!backend
        .probe(&f.repo_root, Some(&context))
        .expect("probe")
        .available);
    assert_eq!(*loads.lock().unwrap_or_else(|poison| poison.into_inner()), 0);
}
