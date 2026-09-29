//! Replace tool arguments with a shallow-merged copy instead of mutating in place.

use serde_json::{Map, Value};

pub fn replace_tool_args(args: &mut Map<String, Value>, patch: &Map<String, Value>) {
    let mut next = args.clone();
    for (key, value) in patch {
        next.insert(key.clone(), value.clone());
    }
    *args = next;
}
