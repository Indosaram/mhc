//! Included only by the two account-crate targets that touch the imagegen process globals
//! (`native_imagegen.rs` reads the native-bypass flag, `native_openai_imagegen.rs` writes it).
//! Each integration test is its own binary, so this lock is per-binary, never cross-binary.

static NATIVE_GLOBAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct GlobalNativeStateGuard {
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl GlobalNativeStateGuard {
    pub async fn acquire() -> Self {
        let guard = NATIVE_GLOBAL_LOCK.lock().await;
        maho_ext_imagegen::state::set_native_bypass(false);
        maho_ext_imagegen::state::set_image_gen_registry_override(None);
        Self { _guard: guard }
    }
}

impl Drop for GlobalNativeStateGuard {
    fn drop(&mut self) {
        maho_ext_imagegen::state::set_native_bypass(false);
        maho_ext_imagegen::state::set_image_gen_registry_override(None);
    }
}
