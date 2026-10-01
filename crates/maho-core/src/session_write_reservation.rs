//! Port of senpi packages/coding-agent/src/core/session-write-reservation.ts.

use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Installed only inside a shared-host session isolate, before constructing any writer.
type Reservation = Box<dyn Fn(&str) + Send + Sync>;

static RESERVE: OnceLock<Reservation> = OnceLock::new();

pub fn install_session_write_reservation(reservation: impl Fn(&str) + Send + Sync + 'static) -> Result<(), String> {
    RESERVE
        .set(Box::new(reservation))
        .map_err(|_| "Session write reservation already installed".to_owned())
}

/// Synchronous SessionManager entry points must obtain the host grant before touching a writer.
pub fn reserve_session_write(path: &str) {
    if let Some(reserve) = RESERVE.get() {
        reserve(path);
    }
}

/// A session writer whose grant must live exactly as long as the writer itself.
pub trait SessionWriterOwner: Send + Sync {
    fn get_session_file(&self) -> Option<String>;
    fn is_persisted(&self) -> bool;
}

type Writer = Weak<dyn SessionWriterOwner>;

fn live_writers() -> &'static Mutex<Vec<Writer>> {
    static WRITERS: OnceLock<Mutex<Vec<Writer>>> = OnceLock::new();
    WRITERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock() -> std::sync::MutexGuard<'static, Vec<Writer>> {
    live_writers().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn same_owner(candidate: &Arc<dyn SessionWriterOwner>, owner: &Arc<dyn SessionWriterOwner>) -> bool {
    Arc::ptr_eq(candidate, owner)
}

pub fn register_session_writer(owner: &Arc<dyn SessionWriterOwner>) {
    let mut writers = lock();
    if writers.iter().any(|weak| weak.ptr_eq(&Arc::downgrade(owner))) {
        return;
    }
    writers.push(Arc::downgrade(owner));
}

pub fn unregister_session_writer(owner: &Arc<dyn SessionWriterOwner>) {
    let mut writers = lock();
    writers.retain(|weak| match weak.upgrade() {
        Some(candidate) => !same_owner(&candidate, owner),
        None => false,
    });
}

/// Whether a persisted writer other than self still owns path; collected writers are pruned here.
pub fn has_other_live_session_writer(path: &str, self_owner: &Arc<dyn SessionWriterOwner>) -> bool {
    let mut writers = lock();
    writers.retain(|weak| weak.strong_count() > 0);
    for weak in writers.iter() {
        let Some(owner) = weak.upgrade() else { continue };
        if same_owner(&owner, self_owner) {
            continue;
        }
        if owner.is_persisted() && owner.get_session_file().as_deref() == Some(path) {
            return true;
        }
    }
    false
}

/// Session files still owned by a live persisted writer; collected writers are pruned here.
pub fn live_session_write_paths() -> Vec<String> {
    let mut writers = lock();
    writers.retain(|weak| weak.strong_count() > 0);
    let mut paths = Vec::new();
    for weak in writers.iter() {
        let Some(owner) = weak.upgrade() else { continue };
        if owner.is_persisted()
            && let Some(path) = owner.get_session_file()
        {
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Writer {
        file: Option<String>,
        persisted: bool,
    }

    impl SessionWriterOwner for Writer {
        fn get_session_file(&self) -> Option<String> {
            self.file.clone()
        }
        fn is_persisted(&self) -> bool {
            self.persisted
        }
    }

    fn writer(file: Option<&str>, persisted: bool) -> Arc<dyn SessionWriterOwner> {
        Arc::new(Writer { file: file.map(str::to_owned), persisted })
    }

    #[test]
    fn another_live_writer_is_detected_and_then_removed() {
        let self_owner = writer(Some("/s.jsonl"), true);
        let other = writer(Some("/s.jsonl"), true);
        register_session_writer(&other);
        assert!(has_other_live_session_writer("/s.jsonl", &self_owner));
        unregister_session_writer(&other);
        assert!(!has_other_live_session_writer("/s.jsonl", &self_owner));
    }

    #[test]
    fn the_self_owner_is_not_counted_as_another() {
        let self_owner = writer(Some("/s.jsonl"), true);
        register_session_writer(&self_owner);
        assert!(!has_other_live_session_writer("/s.jsonl", &self_owner));
        unregister_session_writer(&self_owner);
    }

    #[test]
    fn a_dropped_writer_is_pruned() {
        let self_owner = writer(Some("/s.jsonl"), true);
        {
            let other = writer(Some("/s.jsonl"), true);
            register_session_writer(&other);
        }
        assert!(!has_other_live_session_writer("/s.jsonl", &self_owner));
    }

    #[test]
    fn live_paths_list_persisted_writers_only() {
        let persisted = writer(Some("/a.jsonl"), true);
        let memory = writer(Some("/b.jsonl"), false);
        register_session_writer(&persisted);
        register_session_writer(&memory);
        let paths = live_session_write_paths();
        assert!(paths.contains(&"/a.jsonl".to_owned()));
        assert!(!paths.contains(&"/b.jsonl".to_owned()));
        unregister_session_writer(&persisted);
        unregister_session_writer(&memory);
    }

    #[test]
    fn reserving_without_an_install_is_a_noop() {
        reserve_session_write("/any");
    }
}
