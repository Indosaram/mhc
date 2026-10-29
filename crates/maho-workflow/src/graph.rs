//! Derived graph data (spec §1.2): waves, edges, critical path and bottlenecks of a valid
//! definition.
use crate::definition::WorkflowDefinition;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bottleneck {
    pub id: String,
    /// Number of transitive dependents.
    pub dependents: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphInfo {
    /// Node ids by wave (wave 1 first); ids inside a wave sorted.
    pub waves: Vec<Vec<String>>,
    pub edges: Vec<Edge>,
    pub critical_path: Vec<String>,
    pub bottlenecks: Vec<Bottleneck>,
}

/// Dependencies of each node (deduplicated, known ids only, no self edges).
pub fn dependency_map(definition: &WorkflowDefinition) -> BTreeMap<String, BTreeSet<String>> {
    let ids: BTreeSet<&str> = definition.nodes.iter().map(|n| n.id.as_str()).collect();
    definition
        .nodes
        .iter()
        .map(|node| {
            let deps = node
                .depends_on
                .iter()
                .filter(|dep| *dep != &node.id && ids.contains(dep.as_str()))
                .cloned()
                .collect();
            (node.id.clone(), deps)
        })
        .collect()
}

/// Every node that depends on one of `roots`, directly or transitively (roots excluded).
pub fn downstream(deps: &BTreeMap<String, BTreeSet<String>>, roots: &BTreeSet<String>) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut frontier: Vec<String> = roots.iter().cloned().collect();
    while let Some(current) = frontier.pop() {
        for (id, node_deps) in deps {
            if node_deps.contains(&current) && !roots.contains(id) && found.insert(id.clone()) {
                frontier.push(id.clone());
            }
        }
    }
    found
}

/// Wave number (1-based) of each node.
pub fn wave_numbers(deps: &BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, usize> {
    let mut waves: BTreeMap<String, usize> = BTreeMap::new();
    // Repeated relaxation; a valid definition is acyclic so this ends within `len` passes.
    for _ in 0..=deps.len() {
        let mut changed = false;
        for (id, node_deps) in deps {
            if waves.contains_key(id) {
                continue;
            }
            if node_deps.iter().all(|dep| waves.contains_key(dep)) {
                let wave = node_deps.iter().filter_map(|dep| waves.get(dep)).max().copied().unwrap_or(0) + 1;
                waves.insert(id.clone(), wave);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    waves
}

fn better(candidate: &[String], current: &[String]) -> bool {
    candidate.len() > current.len() || (candidate.len() == current.len() && candidate < current)
}

impl GraphInfo {
    pub fn of(definition: &WorkflowDefinition) -> Self {
        let deps = dependency_map(definition);
        let numbers = wave_numbers(&deps);
        let count = numbers.values().max().copied().unwrap_or(0);
        let mut waves: Vec<Vec<String>> = vec![Vec::new(); count];
        for (id, wave) in &numbers {
            waves[wave - 1].push(id.clone());
        }
        let edges = definition
            .nodes
            .iter()
            .flat_map(|node| {
                deps.get(&node.id).into_iter().flatten().map(|dep| Edge {
                    from: dep.clone(),
                    to: node.id.clone(),
                })
            })
            .collect();
        // Longest chain ending at each node, processed wave by wave.
        let mut best: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for wave in &waves {
            for id in wave {
                let mut chain: Vec<String> = Vec::new();
                for dep in deps.get(id).into_iter().flatten() {
                    if let Some(candidate) = best.get(dep)
                        && (chain.is_empty() || better(candidate, &chain))
                    {
                        chain.clone_from(candidate);
                    }
                }
                chain.push(id.clone());
                best.insert(id.clone(), chain);
            }
        }
        let mut critical_path: Vec<String> = Vec::new();
        for chain in best.values() {
            if critical_path.is_empty() || better(chain, &critical_path) {
                critical_path.clone_from(chain);
            }
        }
        let mut bottlenecks: Vec<Bottleneck> = deps
            .keys()
            .map(|id| Bottleneck {
                id: id.clone(),
                dependents: downstream(&deps, &BTreeSet::from([id.clone()])).len(),
            })
            .collect();
        bottlenecks.sort_by(|a, b| b.dependents.cmp(&a.dependents).then(a.id.cmp(&b.id)));
        Self {
            waves,
            edges,
            critical_path,
            bottlenecks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::node;

    fn def(nodes: Vec<crate::definition::WorkflowNode>) -> WorkflowDefinition {
        WorkflowDefinition {
            key: "k".into(),
            name: "n".into(),
            nodes,
        }
    }

    #[test]
    fn waves_edges_path_bottlenecks() {
        let d = def(vec![
            node("d", &["b1", "b2", "b3"]),
            node("a", &[]),
            node("b2", &["a"]),
            node("b1", &["a"]),
            node("b3", &["a"]),
            node("solo", &[]),
        ]);
        let info = GraphInfo::of(&d);
        assert_eq!(
            info.waves,
            vec![vec!["a".to_owned(), "solo".into()], vec!["b1".into(), "b2".into(), "b3".into()], vec!["d".into()]]
        );
        assert_eq!(info.edges.len(), 6);
        assert_eq!(info.critical_path, ["a", "b1", "d"]);
        assert_eq!(info.bottlenecks[0], Bottleneck { id: "a".into(), dependents: 4 });
        assert_eq!(info.bottlenecks[1].id, "b1");
        assert_eq!(info.bottlenecks.last().map(|b| b.id.as_str()), Some("solo"));
    }
}
