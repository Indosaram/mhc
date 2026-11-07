use super::{JsonValue, Op, Result, apply_one};

pub fn apply_immutable_trusted(target: &JsonValue, operations: &[Op]) -> Result<JsonValue> {
    let mut root = target.clone();
    for operation in operations {
        apply_one(&mut root, operation, false)?;
    }
    Ok(root)
}
