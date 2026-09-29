use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

pub fn find_model_catalog_cycles(catalog: &Map<String, Value>) -> Vec<String> {
    let mut cycle_names: BTreeSet<String> = BTreeSet::new();
    let mut path: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();

    for name in catalog.keys() {
        visit(
            name,
            catalog,
            &mut path,
            &mut visited,
            &mut visiting,
            &mut cycle_names,
        );
    }

    cycle_names.into_iter().collect()
}

fn visit(
    name: &str,
    catalog: &Map<String, Value>,
    path: &mut Vec<String>,
    visited: &mut HashSet<String>,
    visiting: &mut HashSet<String>,
    cycle_names: &mut BTreeSet<String>,
) {
    if visited.contains(name) {
        return;
    }

    visiting.insert(name.to_string());
    path.push(name.to_string());

    let next = catalog
        .get(name)
        .and_then(|entry| entry.get("model"))
        .and_then(Value::as_str);
    if let Some(next) = next
        && catalog.contains_key(next)
    {
        if visiting.contains(next) {
            let cycle_start = path
                .iter()
                .position(|segment| segment.as_str() == next)
                .unwrap_or(0);
            for cycle_name in &path[cycle_start..] {
                cycle_names.insert(cycle_name.clone());
            }
        } else {
            visit(next, catalog, path, visited, visiting, cycle_names);
        }
    }

    path.pop();
    visiting.remove(name);
    visited.insert(name.to_string());
}
