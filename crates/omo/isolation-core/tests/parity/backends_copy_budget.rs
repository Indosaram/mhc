use std::path::Path;
use std::sync::Arc;

use isolation_core::{copy_budget, SpaceFn};

#[test]
fn copy_budget_enforces_free_space_plus_ten_percent_and_the_default_2_gib_during_traversal() {
    let space: SpaceFn = Arc::new(|_path: &Path| Ok((110, 1)));
    let mut budget = copy_budget(Path::new("unused"), None, Some(space)).expect("budget");
    budget.consume(100).expect("within budget");
    let error = budget.consume(1).expect_err("must fail");
    assert!(error.is_unavailable());

    let ceiling_space: SpaceFn = Arc::new(|_path: &Path| Ok((10 * 1024 * 1024 * 1024, 1)));
    let mut ceiling = copy_budget(Path::new("unused"), None, Some(ceiling_space)).expect("budget");
    let error = ceiling
        .consume(2 * 1024 * 1024 * 1024 + 1)
        .expect_err("must fail");
    assert!(error.to_string().contains("maxCopyBytes"));
}
