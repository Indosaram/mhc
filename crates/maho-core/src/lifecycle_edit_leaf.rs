//! Optimistic transcript leaves ignore only lifecycle bookkeeping.
use crate::session_manager::SessionManager;
pub fn lifecycle_edit_leaf(manager: &SessionManager, expected: Option<&str>) -> Option<String> {
    let current = manager.leaf_id().map(str::to_owned);
    let Some(expected) = expected else {
        return current;
    };
    let mut leaf = current.clone();
    while let Some(id) = leaf.as_deref() {
        if id == expected {
            return Some(expected.into());
        }
        let Some(e) = manager.entry(id) else {
            return current;
        };
        if e["type"] != "custom"
            || !["session-lifecycle", "work-custody", "child-lifecycle"]
                .contains(&e["customType"].as_str().unwrap_or(""))
        {
            return current;
        }
        leaf = e["parentId"].as_str().map(str::to_owned);
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn skips_only_bookkeeping() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let expected = manager.append_custom("caller", Some(json!({})))["id"]
            .as_str()
            .expect("id")
            .to_owned();
        manager.append_custom("session-lifecycle", Some(json!({})));
        manager.append_custom("work-custody", Some(json!({})));
        assert_eq!(
            lifecycle_edit_leaf(&manager, Some(&expected)),
            Some(expected.clone())
        );
        manager.append_custom("other", Some(json!({})));
        assert_ne!(
            lifecycle_edit_leaf(&manager, Some(&expected)),
            Some(expected)
        );
    }
}
