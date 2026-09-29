//! `tools/task/validation.ts`: category XOR subagent_type target selection and batch shape.

use crate::tools::task::types::{ResolvedSpawnItem, SpawnTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskTargetErrorCode {
    BothTargets,
    NoTarget,
    CategoryWithModel,
}

impl TaskTargetErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BothTargets => "both_targets",
            Self::NoTarget => "no_target",
            Self::CategoryWithModel => "category_with_model",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTargetError {
    pub code: TaskTargetErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskTargetSelection {
    Category(String),
    SubagentType(String),
    Error(TaskTargetError),
}

/// The target fields every spawn input carries.
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetInput<'a> {
    pub category: Option<&'a str>,
    pub subagent_type: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// One `tasks[]` entry of a batch spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnItemInput {
    pub prompt: String,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub name: Option<String>,
    pub load_skills: Option<Vec<String>>,
}

/// The spawn-shaped subset of the task tool params.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnParamsInput {
    pub prompt: Option<String>,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub name: Option<String>,
    pub load_skills: Option<Vec<String>>,
    pub run_in_background: Option<bool>,
    pub tasks: Option<Vec<SpawnItemInput>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchShapeErrorCode {
    PromptAndTasks,
    NoPromptOrTasks,
    EmptyTasks,
}

impl BatchShapeErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PromptAndTasks => "prompt_and_tasks",
            Self::NoPromptOrTasks => "no_prompt_or_tasks",
            Self::EmptyTasks => "empty_tasks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchShapeError {
    pub code: BatchShapeErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchShape {
    Single,
    Batch,
    Error(BatchShapeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveSpawnItemsError {
    Shape(BatchShapeError),
    /// `code: "item_target"`: item `index` failed target validation.
    ItemTarget {
        index: usize,
        message: String,
    },
}

impl ResolveSpawnItemsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Shape(error) => error.code.as_str(),
            Self::ItemTarget { .. } => "item_target",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Shape(error) => &error.message,
            Self::ItemTarget { message, .. } => message,
        }
    }
}

pub const BOTH_TARGETS_MESSAGE: &str =
    "Provide EITHER category OR subagent_type, not both. Remove one and retry.";

pub const CATEGORY_WITH_MODEL_MESSAGE: &str = "Provide EITHER category OR model, never both. A category-routed task always takes its model from the omo.json category config; a call-site model override would silently bypass that routing. Remove model and retry, or use subagent_type for an explicit-model spawn, or configure categories.<name>.models in omo.json.";

pub const NO_TARGET_MESSAGE: &str = "You MUST provide EITHER category OR subagent_type. Omitting BOTH will FAIL. Example: task(category=\"quick\", prompt=\"...\") or task(subagent_type=\"momus\", prompt=\"...\").";

const PROMPT_AND_TASKS_MESSAGE: &str =
    "Provide EITHER prompt OR tasks, not both. Remove one and retry.";

const NO_PROMPT_OR_TASKS_MESSAGE: &str = "Provide EITHER prompt OR tasks. One field is required.";

const EMPTY_TASKS_MESSAGE: &str = "tasks must contain at least one item.";

fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.trim().is_empty())
}

fn target_error(code: TaskTargetErrorCode, message: &str) -> TaskTargetSelection {
    TaskTargetSelection::Error(TaskTargetError {
        code,
        message: message.to_string(),
    })
}

/// category XOR subagent_type: both or neither is a typed tool error.
pub fn validate_task_target(params: TargetInput<'_>) -> TaskTargetSelection {
    let category = present(params.category);
    let subagent = present(params.subagent_type);
    match (category, subagent) {
        (Some(_), Some(_)) => target_error(TaskTargetErrorCode::BothTargets, BOTH_TARGETS_MESSAGE),
        (Some(_), None) if present(params.model).is_some() => target_error(
            TaskTargetErrorCode::CategoryWithModel,
            CATEGORY_WITH_MODEL_MESSAGE,
        ),
        (Some(category), None) => TaskTargetSelection::Category(category.trim().to_string()),
        (None, Some(subagent)) => TaskTargetSelection::SubagentType(subagent.trim().to_string()),
        (None, None) => target_error(TaskTargetErrorCode::NoTarget, NO_TARGET_MESSAGE),
    }
}

fn shape_error(code: BatchShapeErrorCode, message: &str) -> BatchShape {
    BatchShape::Error(BatchShapeError {
        code,
        message: message.to_string(),
    })
}

pub fn validate_batch_shape(params: &SpawnParamsInput) -> BatchShape {
    match (&params.prompt, &params.tasks) {
        (Some(_), Some(_)) => shape_error(
            BatchShapeErrorCode::PromptAndTasks,
            PROMPT_AND_TASKS_MESSAGE,
        ),
        (None, None) => shape_error(
            BatchShapeErrorCode::NoPromptOrTasks,
            NO_PROMPT_OR_TASKS_MESSAGE,
        ),
        (None, Some(tasks)) if tasks.is_empty() => {
            shape_error(BatchShapeErrorCode::EmptyTasks, EMPTY_TASKS_MESSAGE)
        }
        (None, Some(_)) => BatchShape::Batch,
        (Some(_), None) => BatchShape::Single,
    }
}

fn single_item(params: &SpawnParamsInput, prompt: &str) -> SpawnItemInput {
    SpawnItemInput {
        prompt: prompt.to_string(),
        task_summary: params.task_summary.clone(),
        description: params.description.clone(),
        name: params.name.clone(),
        ..SpawnItemInput::default()
    }
}

/// Resolve the single or batch form into per-item spawns. Items inherit the top-level target,
/// model, and load_skills; an item-level category or subagent_type suppresses the inherited other.
pub fn resolve_spawn_items(
    params: &SpawnParamsInput,
) -> Result<Vec<ResolvedSpawnItem>, ResolveSpawnItemsError> {
    if let BatchShape::Error(error) = validate_batch_shape(params) {
        return Err(ResolveSpawnItemsError::Shape(error));
    }
    let inputs: Vec<SpawnItemInput> = match (&params.tasks, &params.prompt) {
        (Some(tasks), _) => tasks.clone(),
        (None, Some(prompt)) => vec![single_item(params, prompt)],
        (None, None) => Vec::new(),
    };
    let mut items = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.into_iter().enumerate() {
        let (category, subagent_type) = match (&input.category, &input.subagent_type) {
            (None, None) => (params.category.as_deref(), params.subagent_type.as_deref()),
            (category, subagent) => (category.as_deref(), subagent.as_deref()),
        };
        let model = input.model.clone().or_else(|| params.model.clone());
        let target = match validate_task_target(TargetInput {
            category,
            subagent_type,
            model: model.as_deref(),
        }) {
            TaskTargetSelection::Category(category) => SpawnTarget::Category(category),
            TaskTargetSelection::SubagentType(subagent) => SpawnTarget::SubagentType(subagent),
            TaskTargetSelection::Error(error) => {
                return Err(ResolveSpawnItemsError::ItemTarget {
                    index,
                    message: format!("Task item {index}: {}", error.message),
                });
            }
        };
        items.push(ResolvedSpawnItem {
            prompt: input.prompt,
            task_summary: input.task_summary,
            description: input.description,
            name: input.name,
            model,
            load_skills: input
                .load_skills
                .or_else(|| params.load_skills.clone())
                .unwrap_or_default(),
            target,
        });
    }
    Ok(items)
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
