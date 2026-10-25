//! Session lifecycle wire projection shared by classic and hosted subscriptions.
use serde_json::Value;
pub fn session_record(mut record: Value) -> Value {
    if record["type"] == "session_lifecycle"
        && let Some(object) = record.as_object_mut()
    {
        object.remove("sessionId");
    }
    record
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn lifecycle_loses_durable_id_but_other_records_keep_theirs() {
        assert_eq!(
            session_record(
                json!({"type":"session_lifecycle","sessionId":"durable","state":"idle"})
            ),
            json!({"type":"session_lifecycle","state":"idle"})
        );
        let ordinary = json!({"type":"entry_appended","sessionId":"s"});
        assert_eq!(session_record(ordinary.clone()), ordinary);
    }
}
