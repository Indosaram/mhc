use super::{apply_one, JsonValue, Op, Result};

pub fn apply_immutable_trusted(target: &JsonValue, operations: &[Op]) -> Result<JsonValue> {
    let mut root = target.clone();
    for operation in operations {
        apply_one(&mut root, operation, false)?;
    }
    Ok(root)
}
