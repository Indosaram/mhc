use std::path::{Path, PathBuf};
use std::sync::Arc;

use isolation_core::{choose_base_dir, resolve_path, sha1_hex, BaseDirIo, IsolationError, IsolationResult};

type StatFn = Arc<dyn Fn(&Path) -> IsolationResult<u64> + Send + Sync>;
type WritableFn = Arc<dyn Fn(&Path) -> IsolationResult<bool> + Send + Sync>;

struct FakeIo {
    stat: StatFn,
    writable: WritableFn,
}

impl BaseDirIo for FakeIo {
    fn stat_dev(&self, path: &Path) -> IsolationResult<u64> {
        (self.stat)(path)
    }

    fn writable(&self, path: &Path) -> IsolationResult<bool> {
        (self.writable)(path)
    }
}

fn home() -> PathBuf {
    resolve_path(Path::new("/home/test"))
}

fn repo() -> PathBuf {
    resolve_path(Path::new("/volume/repo"))
}

fn volume() -> PathBuf {
    resolve_path(Path::new("/volume"))
}

fn segment(id: &str) -> String {
    let repo = repo();
    format!("t{}", &sha1_hex(&format!("{}{}", repo.to_string_lossy(), id))[..10])
}

fn on_volume(path: &Path) -> bool {
    let resolved = resolve_path(path);
    resolved == volume() || resolved.starts_with(volume())
}

#[test]
fn chooses_the_home_directory_on_the_same_device() {
    let io = FakeIo {
        stat: Arc::new(|_| Ok(1)),
        writable: Arc::new(|_| {
            Err(IsolationError::other(
                "same-device selection must not probe volume",
            ))
        }),
    };
    let selection = choose_base_dir(&repo(), &home(), "task-one", &io).expect("selection");
    assert_eq!(selection.base_dir, home().join(".omo").join("wt").join(segment("task-one")));
    assert!(!selection.cross_device);
}

#[test]
fn chooses_the_mount_root_on_a_different_writable_device() {
    let volume = volume();
    let io = FakeIo {
        stat: Arc::new(move |path| Ok(if on_volume(path) { 2 } else { 1 })),
        writable: Arc::new({
            let volume = volume.clone();
            move |path| Ok(resolve_path(path) == volume.join(".omo-wt"))
        }),
    };
    let selection = choose_base_dir(&repo(), &home(), "task-one", &io).expect("selection");
    assert_eq!(
        selection.base_dir,
        volume.join(".omo-wt").join(segment("task-one"))
    );
    assert!(!selection.cross_device);
}

#[test]
fn flags_cross_device_home_fallback_when_volume_is_not_writable() {
    let io = FakeIo {
        stat: Arc::new(|path| Ok(if on_volume(path) { 2 } else { 1 })),
        writable: Arc::new(|_| Ok(false)),
    };
    let selection = choose_base_dir(&repo(), &home(), "task-one", &io).expect("selection");
    assert_eq!(selection.base_dir, home().join(".omo").join("wt").join(segment("task-one")));
    assert!(selection.cross_device);
}

#[test]
fn different_task_ids_get_different_paths_without_interpolating_the_id() {
    let io = FakeIo {
        stat: Arc::new(|_| Ok(1)),
        writable: Arc::new(|_| Ok(true)),
    };
    let first = choose_base_dir(&repo(), &home(), "../escape", &io).expect("first");
    let second = choose_base_dir(&repo(), &home(), "other", &io).expect("second");
    assert_ne!(first.base_dir, second.base_dir);
    assert!(first
        .base_dir
        .starts_with(home().join(".omo").join("wt")));
    let name = first
        .base_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert_eq!(name.len(), 11);
    assert!(name.starts_with('t'));
    assert!(name[1..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert!(!first.base_dir.to_string_lossy().contains("escape"));
}

#[test]
fn never_places_the_base_directory_inside_a_subvolume_style_repository_root() {
    let repo = repo();
    let io = FakeIo {
        stat: Arc::new({ let repo = repo.clone(); move |path| {
            Ok(if resolve_path(path) == repo {
                7
            } else if on_volume(path) {
                9
            } else {
                1
            })
        }}),
        writable: Arc::new(|_| Ok(true)),
    };
    let selection = choose_base_dir(&repo, &home(), "task-one", &io).expect("selection");
    assert!(!selection
        .base_dir
        .to_string_lossy()
        .contains(&repo.to_string_lossy().into_owned()));
    assert_eq!(
        selection.base_dir,
        volume().join(".omo-wt").join(segment("task-one"))
    );
    assert!(!selection.cross_device);
}

#[test]
fn falls_back_home_cross_device_when_a_subvolume_root_has_no_writable_ancestor() {
    let repo = repo();
    let io = FakeIo {
        stat: Arc::new({
            let repo = repo.clone();
            move |path| {
                Ok(if resolve_path(path) == repo {
                    7
                } else if on_volume(path) {
                    9
                } else {
                    1
                })
            }
        }),
        writable: Arc::new({
            let repo = repo.clone();
            move |path| Ok(resolve_path(path).starts_with(&repo))
        }),
    };
    let selection = choose_base_dir(&repo, &home(), "task-one", &io).expect("selection");
    assert!(!selection
        .base_dir
        .to_string_lossy()
        .contains(&repo.to_string_lossy().into_owned()));
    assert_eq!(selection.base_dir, home().join(".omo").join("wt").join(segment("task-one")));
    assert!(selection.cross_device);
}
