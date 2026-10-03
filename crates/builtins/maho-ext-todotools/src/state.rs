pub use crate::todo_format::*;
pub use crate::todo_operations::*;
pub use crate::todo_query::*;
pub use crate::todo_resolution::*;
pub use crate::todo_storage::*;
pub use crate::todo_types::*;
pub use crate::todo_widget::*;
pub fn clone_phases(phases:&[TodoPhase])->Vec<TodoPhase> { phases.to_vec() }
pub fn clone_task(task:&TodoItem)->TodoItem { task.clone() }
