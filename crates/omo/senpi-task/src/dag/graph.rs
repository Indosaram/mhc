//! `dag/graph.ts`: pure DAG compilation (validation, waves, critical path, bottlenecks).
// allow: SIZE_OK - one pure compiler mirroring graph.ts; validation, waves, critical path and bottlenecks share private state.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::dag::types::{
    DAG_SETTINGS_DEFAULTS, DagBottleneck, DagDiagnostic, DagEdge, DagNode, DagNodeId, DagNodeState,
    DagNodeTarget, DagRoute, DagSettings, DagWave,
};

/// Scheduling-only dependency graph input: dependsOn orders execution and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagNodeInput {
    pub id: String,
    pub prompt: String,
    pub target: DagNodeTarget,
    pub label: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub load_skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagDefinition {
    pub key: String,
    pub name: String,
    pub nodes: Vec<DagNodeInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DagCompileErrorCode {
    DuplicateNodeId,
    UnknownDependency,
    SelfDependency,
    Cycle,
    NodeCountExceeded,
    PromptBytesExceeded,
    DependencyFanoutExceeded,
}

impl DagCompileErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateNodeId => "duplicate_node_id",
            Self::UnknownDependency => "unknown_dependency",
            Self::SelfDependency => "self_dependency",
            Self::Cycle => "cycle",
            Self::NodeCountExceeded => "node_count_exceeded",
            Self::PromptBytesExceeded => "prompt_bytes_exceeded",
            Self::DependencyFanoutExceeded => "dependency_fanout_exceeded",
        }
    }
}

/// Every compile error is fatal: when errors is non-empty no graph is produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagCompileError {
    pub code: DagCompileErrorCode,
    pub message: String,
    pub node_ids: Vec<DagNodeId>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DagCompileResult {
    pub ok: bool,
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    pub waves: Vec<DagWave>,
    pub critical_path: Vec<DagNodeId>,
    pub bottlenecks: Vec<DagBottleneck>,
    pub diagnostics: Vec<DagDiagnostic>,
    pub errors: Vec<DagCompileError>,
}

/// `at` is caller-supplied so compilation stays pure.
#[derive(Debug, Clone, Default)]
pub struct DagCompileOptions {
    pub at: Option<String>,
    pub settings: Option<DagSettings>,
}

const EMPTY_AT: &str = "1970-01-01T00:00:00.000Z";

/// Longer sequences first, then lexicographic by id.
fn compare_sequences(a: &[DagNodeId], b: &[DagNodeId]) -> std::cmp::Ordering {
    b.len().cmp(&a.len()).then_with(|| a.cmp(b))
}

pub fn route_of(target: &DagNodeTarget) -> DagRoute {
    match target {
        DagNodeTarget::Category(category) => DagRoute::Category {
            category: category.clone(),
        },
        DagNodeTarget::SubagentType {
            subagent_type,
            model,
        } => DagRoute::Agent {
            agent: subagent_type.clone(),
            model: model.clone(),
        },
    }
}

fn failure(errors: Vec<DagCompileError>, at: &str) -> DagCompileResult {
    let diagnostics = errors
        .iter()
        .map(|error| match error.node_ids.as_slice() {
            [node_id] => DagDiagnostic::NodeFlag {
                node_id: node_id.clone(),
                message: error.message.clone(),
                at: at.to_string(),
            },
            _ => DagDiagnostic::RunFlag {
                message: error.message.clone(),
                at: at.to_string(),
            },
        })
        .collect();
    DagCompileResult {
        ok: false,
        diagnostics,
        errors,
        ..DagCompileResult::default()
    }
}

type Dependencies = HashMap<DagNodeId, Vec<DagNodeId>>;

/// Deterministic cycle listing: walk from the smallest member of each entangled region, always
/// stepping to the smallest dependency that re-enters the region.
fn find_cycles(ids: &[DagNodeId], dependencies: &Dependencies) -> Vec<Vec<DagNodeId>> {
    let mut remaining: BTreeSet<DagNodeId> = ids.iter().cloned().collect();
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot: Vec<DagNodeId> = remaining.iter().cloned().collect();
        for id in snapshot {
            let deps = dependencies.get(&id).map(Vec::as_slice).unwrap_or_default();
            if deps.iter().all(|dep| !remaining.contains(dep)) {
                remaining.remove(&id);
                changed = true;
            }
        }
    }
    let mut cycles = Vec::new();
    let mut consumed: HashSet<DagNodeId> = HashSet::new();
    for start in &remaining {
        if consumed.contains(start) {
            continue;
        }
        let mut path = vec![start.clone()];
        let mut seen: HashMap<DagNodeId, usize> = HashMap::from([(start.clone(), 0)]);
        let mut current = start.clone();
        loop {
            let next = dependencies
                .get(&current)
                .into_iter()
                .flatten()
                .filter(|dep| remaining.contains(*dep))
                .min()
                .cloned();
            let Some(next) = next else { break };
            if let Some(&seen_at) = seen.get(&next) {
                let members = &path[seen_at..];
                consumed.extend(members.iter().cloned());
                // path follows dependency edges backwards; report it in execution order, closed.
                let mut ordered = vec![members[0].clone()];
                ordered.extend(members[1..].iter().rev().cloned());
                ordered.push(members[0].clone());
                cycles.push(ordered);
                break;
            }
            seen.insert(next.clone(), path.len());
            path.push(next.clone());
            current = next;
        }
    }
    cycles
}

fn validate(
    definition: &DagDefinition,
    settings: &DagSettings,
) -> (Vec<DagCompileError>, HashMap<DagNodeId, usize>) {
    let mut errors = Vec::new();
    if definition.nodes.len() > settings.max_nodes_per_run {
        errors.push(DagCompileError {
            code: DagCompileErrorCode::NodeCountExceeded,
            message: format!(
                "dag has {} nodes, exceeding max_nodes_per_run {}",
                definition.nodes.len(),
                settings.max_nodes_per_run
            ),
            node_ids: Vec::new(),
        });
    }
    let mut declaration_index = HashMap::new();
    let mut duplicates: Vec<DagNodeId> = Vec::new();
    for (index, input) in definition.nodes.iter().enumerate() {
        if declaration_index.contains_key(&input.id) {
            if !duplicates.contains(&input.id) {
                duplicates.push(input.id.clone());
            }
            continue;
        }
        declaration_index.insert(input.id.clone(), index);
    }
    for duplicate in duplicates {
        errors.push(DagCompileError {
            code: DagCompileErrorCode::DuplicateNodeId,
            message: format!("duplicate node id \"{duplicate}\""),
            node_ids: vec![duplicate],
        });
    }
    for input in &definition.nodes {
        let id = &input.id;
        let bytes = input.prompt.len();
        if bytes > settings.max_prompt_bytes {
            errors.push(DagCompileError {
                code: DagCompileErrorCode::PromptBytesExceeded,
                message: format!(
                    "node \"{id}\" prompt is {bytes} bytes, exceeding max_prompt_bytes {}",
                    settings.max_prompt_bytes
                ),
                node_ids: vec![id.clone()],
            });
        }
        let deps = input.depends_on.as_deref().unwrap_or_default();
        if deps.len() > settings.max_nodes_per_run {
            errors.push(DagCompileError {
                code: DagCompileErrorCode::DependencyFanoutExceeded,
                message: format!(
                    "node \"{id}\" depends on {} nodes, exceeding max_nodes_per_run {}",
                    deps.len(),
                    settings.max_nodes_per_run
                ),
                node_ids: vec![id.clone()],
            });
        }
        for dep in deps {
            if dep == id {
                errors.push(DagCompileError {
                    code: DagCompileErrorCode::SelfDependency,
                    message: format!("node \"{id}\" depends on itself"),
                    node_ids: vec![id.clone()],
                });
            } else if !declaration_index.contains_key(dep) {
                errors.push(DagCompileError {
                    code: DagCompileErrorCode::UnknownDependency,
                    message: format!("node \"{id}\" depends on unknown node \"{dep}\""),
                    node_ids: vec![id.clone(), dep.clone()],
                });
            }
        }
    }
    (errors, declaration_index)
}

pub fn compile_dag(definition: &DagDefinition, options: &DagCompileOptions) -> DagCompileResult {
    let at = options.at.as_deref().unwrap_or(EMPTY_AT);
    let settings = options.settings.unwrap_or(DAG_SETTINGS_DEFAULTS);
    let (mut errors, declaration_index) = validate(definition, &settings);

    let unique_inputs: Vec<&DagNodeInput> = definition
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, input)| declaration_index.get(&input.id) == Some(index))
        .map(|(_, input)| input)
        .collect();
    let ids: Vec<DagNodeId> = unique_inputs.iter().map(|input| input.id.clone()).collect();
    let mut dependencies: Dependencies = HashMap::new();
    for input in &unique_inputs {
        let mut deps: Vec<DagNodeId> = Vec::new();
        for dep in input.depends_on.as_deref().unwrap_or_default() {
            if dep != &input.id && declaration_index.contains_key(dep) && !deps.contains(dep) {
                deps.push(dep.clone());
            }
        }
        dependencies.insert(input.id.clone(), deps);
    }

    if errors.is_empty() {
        for cycle in find_cycles(&ids, &dependencies) {
            errors.push(DagCompileError {
                code: DagCompileErrorCode::Cycle,
                message: format!("dependency cycle detected: {}", cycle.join(" -> ")),
                node_ids: cycle,
            });
        }
    }
    if !errors.is_empty() {
        return failure(errors, at);
    }

    let nodes: Vec<DagNode> = unique_inputs
        .iter()
        .map(|input| DagNode {
            id: input.id.clone(),
            label: input.label.clone(),
            prompt: input.prompt.clone(),
            route: route_of(&input.target),
            depends_on: dependencies.get(&input.id).cloned().unwrap_or_default(),
            state: DagNodeState::Pending,
            task_id: None,
            attempt: 0,
            error: None,
            run_stats: None,
            created_at: at.to_string(),
            started_at: None,
            completed_at: None,
        })
        .collect();
    let declared = |id: &DagNodeId| declaration_index.get(id).copied().unwrap_or(0);

    let mut edges: Vec<DagEdge> = nodes
        .iter()
        .flat_map(|node| {
            node.depends_on.iter().map(|dep| DagEdge {
                from: dep.clone(),
                to: node.id.clone(),
            })
        })
        .collect();
    edges.sort_by(|a, b| {
        declared(&a.to)
            .cmp(&declared(&b.to))
            .then_with(|| declared(&a.from).cmp(&declared(&b.from)))
    });

    let depth = compute_depths(&ids, &dependencies);
    let mut waves_by_index: BTreeMap<usize, Vec<DagNodeId>> = BTreeMap::new();
    for id in &ids {
        waves_by_index
            .entry(depth.get(id).copied().unwrap_or(0))
            .or_default()
            .push(id.clone());
    }
    let waves = waves_by_index
        .into_iter()
        .map(|(index, mut node_ids)| {
            node_ids.sort_by(|a, b| declared(a).cmp(&declared(b)).then_with(|| a.cmp(b)));
            DagWave { index, node_ids }
        })
        .collect();

    let mut dependents: HashMap<DagNodeId, Vec<DagNodeId>> =
        ids.iter().map(|id| (id.clone(), Vec::new())).collect();
    for edge in &edges {
        if let Some(list) = dependents.get_mut(&edge.from) {
            list.push(edge.to.clone());
        }
    }

    DagCompileResult {
        ok: true,
        nodes,
        edges,
        waves,
        critical_path: critical_path(&ids, &dependents),
        bottlenecks: bottlenecks(&ids, &dependents),
        diagnostics: Vec::new(),
        errors: Vec::new(),
    }
}

/// Longest-dependency depth, computed in topological order (the graph is acyclic here).
fn compute_depths(ids: &[DagNodeId], dependencies: &Dependencies) -> HashMap<DagNodeId, usize> {
    let mut depth: HashMap<DagNodeId, usize> = HashMap::new();
    while depth.len() < ids.len() {
        let before = depth.len();
        for id in ids {
            if depth.contains_key(id) {
                continue;
            }
            let deps = dependencies.get(id).map(Vec::as_slice).unwrap_or_default();
            let known: Option<Vec<usize>> =
                deps.iter().map(|dep| depth.get(dep).copied()).collect();
            if let Some(known) = known {
                let value = known.into_iter().max().map_or(0, |max| max + 1);
                depth.insert(id.clone(), value);
            }
        }
        if depth.len() == before {
            break;
        }
    }
    depth
}

fn reverse_topological(
    ids: &[DagNodeId],
    dependents: &HashMap<DagNodeId, Vec<DagNodeId>>,
) -> Vec<DagNodeId> {
    let mut done: HashSet<DagNodeId> = HashSet::new();
    let mut order = Vec::with_capacity(ids.len());
    while order.len() < ids.len() {
        let before = order.len();
        for id in ids {
            if done.contains(id) {
                continue;
            }
            let successors = dependents.get(id).map(Vec::as_slice).unwrap_or_default();
            if successors.iter().all(|successor| done.contains(successor)) {
                done.insert(id.clone());
                order.push(id.clone());
            }
        }
        if order.len() == before {
            break;
        }
    }
    order
}

fn critical_path(
    ids: &[DagNodeId],
    dependents: &HashMap<DagNodeId, Vec<DagNodeId>>,
) -> Vec<DagNodeId> {
    let mut longest: HashMap<DagNodeId, Vec<DagNodeId>> = HashMap::new();
    for id in reverse_topological(ids, dependents) {
        let mut successors = dependents.get(&id).cloned().unwrap_or_default();
        successors.sort();
        let mut best: &[DagNodeId] = &[];
        for successor in &successors {
            let candidate = longest
                .get(successor)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if best.is_empty() || compare_sequences(candidate, best).is_lt() {
                best = candidate;
            }
        }
        let mut path = vec![id.clone()];
        path.extend_from_slice(best);
        longest.insert(id, path);
    }
    let mut sorted = ids.to_vec();
    sorted.sort();
    let mut path: Vec<DagNodeId> = Vec::new();
    for id in sorted {
        let candidate = longest.get(&id).map(Vec::as_slice).unwrap_or_default();
        if path.is_empty() || compare_sequences(candidate, &path).is_lt() {
            path = candidate.to_vec();
        }
    }
    path
}

fn bottlenecks(
    ids: &[DagNodeId],
    dependents: &HashMap<DagNodeId, Vec<DagNodeId>>,
) -> Vec<DagBottleneck> {
    let mut descendants: HashMap<DagNodeId, HashSet<DagNodeId>> = HashMap::new();
    for id in reverse_topological(ids, dependents) {
        let mut collected = HashSet::new();
        for successor in dependents.get(&id).into_iter().flatten() {
            collected.insert(successor.clone());
            if let Some(transitive) = descendants.get(successor) {
                collected.extend(transitive.iter().cloned());
            }
        }
        descendants.insert(id, collected);
    }
    let mut result: Vec<DagBottleneck> = ids
        .iter()
        .map(|id| DagBottleneck {
            node_id: id.clone(),
            blocked_count: descendants.get(id).map_or(0, HashSet::len),
        })
        .collect();
    result.sort_by(|a, b| {
        b.blocked_count
            .cmp(&a.blocked_count)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    result
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
