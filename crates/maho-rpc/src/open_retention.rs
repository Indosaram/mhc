//! Port of senpi `packages/coding-agent/src/modes/rpc/open-retention.ts`.
//!
//! Whether a session an open loads stays when the opening connection ends, decided at line receipt
//! (senpi#5808, senpi#6267).

use serde_json::{Map, Value};

fn retained_against(asked: Option<&Value>, delegated: bool, machine_owned: bool) -> bool {
    if delegated {
        asked.and_then(Value::as_bool) != Some(true)
    } else {
        machine_owned && asked.is_none_or(Value::is_null)
    }
}

/// Applies open-retention policy to an inbound JSON command (`open_session`, `claim_session`,
/// or `move_session { op: "import", open }`).
pub fn with_open_retention(mut command: Value, delegated: bool, machine_owned: bool) -> Value {
    let Some(obj) = command.as_object_mut() else {
        return command;
    };
    let kind = obj.get("type").and_then(Value::as_str);
    match kind {
        Some("open_session" | "claim_session")
            if retained_against(obj.get("retain_on_disconnect"), delegated, machine_owned) =>
        {
            obj.insert("retain_on_disconnect".to_owned(), Value::Bool(true));
        }
        Some("open_session" | "claim_session") => {}
        Some("move_session") if obj.get("op").and_then(Value::as_str) == Some("import") => {
            let mut fields = match obj.get("open") {
                Some(Value::Bool(true)) => Some(Map::new()),
                Some(Value::Object(map)) => Some(map.clone()),
                _ => None,
            };
            if let Some(ref mut map) = fields
                && retained_against(map.get("retain_on_disconnect"), delegated, machine_owned)
            {
                map.insert("retain_on_disconnect".to_owned(), Value::Bool(true));
                obj.insert("open".to_owned(), Value::Object(map.clone()));
            }
        }
        _ => {}
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delegated_forces_retain_unless_explicit_true_and_handles_import_open() {
        let open = with_open_retention(json!({"type":"open_session","cwd":"/x"}), true, false);
        assert_eq!(open["retain_on_disconnect"], true);

        let claim = with_open_retention(
            json!({"type":"claim_session","retain_on_disconnect":false}),
            true,
            false,
        );
        assert_eq!(claim["retain_on_disconnect"], true);

        let machine = with_open_retention(json!({"type":"open_session"}), false, true);
        assert_eq!(machine["retain_on_disconnect"], true);

        let explicit_false = with_open_retention(
            json!({"type":"open_session","retain_on_disconnect":false}),
            false,
            true,
        );
        assert_eq!(explicit_false["retain_on_disconnect"], false);

        let import_true = with_open_retention(
            json!({"type":"move_session","op":"import","open":true}),
            true,
            false,
        );
        assert_eq!(import_true["open"]["retain_on_disconnect"], true);
    }

    fn import_with(open: Option<Value>) -> Value {
        let mut cmd = json!({"type":"move_session","op":"import","from":"/a"});
        if let Some(o) = open {
            cmd["open"] = o;
        }
        cmd
    }

    #[test]
    fn machine_owned_import_open_retention() {
        assert_eq!(
            with_open_retention(import_with(Some(json!(true))), false, true)["open"],
            json!({"retain_on_disconnect": true})
        );
        assert_eq!(
            with_open_retention(
                import_with(Some(json!({"retain_on_disconnect": false}))),
                false,
                true
            )["open"],
            json!({"retain_on_disconnect": false})
        );
    }

    #[test]
    fn delegated_import_open_forces_retain_on_disconnect() {
        assert_eq!(
            with_open_retention(
                import_with(Some(json!({"retain_on_disconnect": false}))),
                true,
                false
            )["open"],
            json!({"retain_on_disconnect": true})
        );
        for open in [
            json!(true),
            json!({"kind": "worker"}),
            json!({"retain_on_disconnect": false}),
        ] {
            let cmd = import_with(Some(open.clone()));
            assert_eq!(with_open_retention(cmd, false, false)["open"], open);
        }
    }

    #[test]
    fn invalid_import_open_and_other_move_ops_are_left_untouched() {
        for bad in [None, Some(json!("yes")), Some(json!([true]))] {
            let cmd = import_with(bad);
            assert_eq!(with_open_retention(cmd.clone(), false, true), cmd);
        }
        let status = json!({"type":"move_session","op":"status","moveId":"move-1"});
        assert_eq!(with_open_retention(status.clone(), false, true), status);
    }
}
