use isolation_core::copy_tree;
use isolation_core::test_support::fixture;

#[test]
fn copy_tree_does_not_descend_into_its_own_destination_when_it_lives_inside_the_source() {
    let f = fixture();
    let lower = f.root.join("repo");
    std::fs::create_dir_all(lower.join("nested")).expect("nested");
    std::fs::write(lower.join("nested/file"), "content").expect("file");
    // The volume walk places the base directory inside the repository when the
    // repository root is a subvolume with its own st_dev; ensure creates the
    // parent before the backend starts, so the destination already exists.
    let destination = lower.join(".omo-wt/handle/m");
    std::fs::create_dir_all(lower.join(".omo-wt/handle")).expect("handle");
    copy_tree(
        &lower,
        &destination,
        &mut |_source, target, size| {
            if size > 0 {
                std::fs::write(target, "cloned")?;
            }
            Ok(())
        },
        &mut |_size| Ok(()),
    )
    .expect("copy");
    assert_eq!(
        std::fs::read_to_string(destination.join("nested/file")).expect("file"),
        "cloned"
    );
    // The walk must not chase its own output: the destination subtree the
    // source already contains stays un-copied below the new tree.
    assert!(!destination.join(".omo-wt/handle/m").exists());
}
