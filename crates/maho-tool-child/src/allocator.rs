//! Process allocator for the `mhc` binary. glibc malloc keeps the free chunks left behind by
//! short-lived subagent sessions, so a multi-agent session's resident set ratchets upward even
//! after every subagent is dropped and `malloc_trim` runs. On unix the binary uses jemalloc with
//! immediate page return from one arena and single-page small-size slabs, so a slab pinned by one
//! surviving object holds 4 KiB instead of a multi-page run; Windows keeps the system heap.
#![allow(unsafe_code)]

#[cfg(unix)]
pub type MahoAllocator = tikv_jemallocator::Jemalloc;
#[cfg(unix)]
pub const ALLOCATOR: MahoAllocator = tikv_jemallocator::Jemalloc;

#[cfg(not(unix))]
pub type MahoAllocator = std::alloc::System;
#[cfg(not(unix))]
pub const ALLOCATOR: MahoAllocator = std::alloc::System;

/// Read by jemalloc at its first allocation; `_RJEM_MALLOC_CONF` in the environment still overrides it.
#[cfg(unix)]
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static JEMALLOC_CONF: &[u8; 103] =
    b"background_thread:true,dirty_decay_ms:0,muzzy_decay_ms:0,narenas:1,slab_sizes:1-4096:1,tcache_max:1024\0";

/// Flush the calling thread's cache and purge every arena after a large release.
pub fn reclaim_unused_heap() {
    #[cfg(unix)]
    {
        const FLUSH_TCACHE: &[u8] = b"thread.tcache.flush\0";
        const PURGE_ALL_ARENAS: &[u8] = b"arena.4096.purge\0";
        for name in [FLUSH_TCACHE, PURGE_ALL_ARENAS] {
            // SAFETY: `name` is a NUL-terminated mallctl name for a void action that reads and
            // writes no value, so every pointer argument is null and the length is 0.
            unsafe {
                let _ = tikv_jemalloc_sys::mallctl(
                    name.as_ptr().cast(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        unsafe extern "C" {
            fn _heapmin() -> libc::c_int;
        }
        // SAFETY: `_heapmin` releases unused CRT heap segments to the OS.
        unsafe {
            let _ = _heapmin();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[global_allocator]
    static TEST_ALLOCATOR: MahoAllocator = ALLOCATOR;

    fn option<T: Default>(name: &[u8]) -> T {
        let mut value = T::default();
        let mut len = std::mem::size_of::<T>();
        // SAFETY: `name` is NUL-terminated and names an option whose C type has the width of
        // `T`; `value` and `len` describe a writable buffer of that width.
        let status = unsafe {
            tikv_jemalloc_sys::mallctl(
                name.as_ptr().cast(),
                (&mut value as *mut T).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(status, 0, "mallctl {}", String::from_utf8_lossy(name));
        value
    }

    #[test]
    fn embedded_configuration_returns_pages_immediately_from_one_arena() {
        if std::env::var_os("_RJEM_MALLOC_CONF").is_some() {
            return;
        }
        assert_eq!(option::<isize>(b"opt.dirty_decay_ms\0"), 0);
        assert_eq!(option::<isize>(b"opt.muzzy_decay_ms\0"), 0);
        assert_eq!(option::<u32>(b"opt.narenas\0"), 1);
        assert_eq!(option::<usize>(b"opt.tcache_max\0"), 1024);
        assert!(option::<bool>(b"opt.background_thread\0"));
    }

    #[cfg(target_os = "linux")]
    fn resident_kb() -> usize {
        let status = std::fs::read_to_string("/proc/self/status").expect("proc status");
        status
            .lines()
            .find_map(|line| line.strip_prefix("RssAnon:"))
            .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
            .expect("RssAnon")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn freed_transcript_strings_interleaved_with_small_survivors_return_to_the_os() {
        let before = resident_kb();
        let mut survivors: Vec<Box<[u8; 48]>> = Vec::with_capacity(32 * 1024);
        let mut transient: Vec<Vec<u8>> = Vec::with_capacity(32 * 1024);
        for index in 0..32 * 1024 {
            transient.push(vec![index as u8; 2048]);
            survivors.push(Box::new([index as u8; 48]));
        }
        let grown = resident_kb();
        assert!(grown >= before + 48 * 1024, "{before} -> {grown}");
        drop(transient);
        reclaim_unused_heap();
        let after = resident_kb();
        assert!(
            after < before + 16 * 1024,
            "RssAnon stayed at {after} kB with {} small survivors (was {before}, peak {grown})",
            survivors.len()
        );
    }
}
