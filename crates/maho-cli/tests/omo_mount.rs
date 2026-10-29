use std::collections::BTreeMap;

use maho_cli::cli::default_extensions::TaskParent;
use maho_cli::cli::omo_mount::OmoMount;

fn isolated_env(home: &std::path::Path) -> BTreeMap<String, String> {
    BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())])
}

#[tokio::test]
async fn for_parent_composes_retained_task_and_memory_components() {
    let dir = tempfile::tempdir().expect("isolated mount cwd");
    let parent: TaskParent = std::sync::Arc::new(std::sync::OnceLock::new());

    let mount = OmoMount::for_parent(parent, dir.path(), dir.path(), isolated_env(dir.path()))
        .expect("mount builds without packaged resources");

    let names: Vec<&str> = mount.extension().components().iter().map(|component| component.name).collect();
    assert!(names.contains(&"task"), "task component present: {names:?}");
    assert!(names.contains(&"memory"), "memory component present: {names:?}");
}
