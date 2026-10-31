//! The `session_opened` record: the first lifecycle record of every session a host creates
//! (senpi `rpc/session-status-opened.ts`).
use crate::jsonl::serialize_json_line;
use crate::session_status::PublishedSessionContext;
use maho_ext_api::SessionKind;
use serde::Serialize;

/// A session kind as its wire string (`interactive` or `worker`).
pub fn session_kind_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Interactive => "interactive",
        SessionKind::Worker => "worker",
    }
}

#[derive(Clone, Debug)]
pub struct SessionOpenedRecord {
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub kind: SessionKind,
    pub cwd: String,
    pub context: Option<PublishedSessionContext>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Wire<'a> {
    r#type: &'static str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    durable_session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_path: Option<&'a str>,
    kind: &'static str,
    cwd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'a PublishedSessionContext>,
}

/// The two wire forms: `plain` for every recipient, `with_context` for subscribers that asked for context.
pub struct SessionOpenedLines {
    pub plain: String,
    pub with_context: String,
}

pub fn session_opened_lines(
    record: &SessionOpenedRecord,
) -> Result<SessionOpenedLines, serde_json::Error> {
    let mut wire = Wire {
        r#type: "session_opened",
        session_id: &record.session_id,
        durable_session_id: record.durable_session_id.as_deref(),
        session_path: record.session_path.as_deref(),
        kind: session_kind_name(record.kind),
        cwd: &record.cwd,
        context: None,
    };
    let plain = serialize_json_line(&wire)?;
    wire.context = record.context.as_ref();
    Ok(SessionOpenedLines {
        plain,
        with_context: serialize_json_line(&wire)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_travels_only_in_the_context_form() {
        let record = SessionOpenedRecord {
            session_id: "s1".into(),
            durable_session_id: Some("d1".into()),
            session_path: None,
            kind: SessionKind::Worker,
            cwd: "/w".into(),
            context: Some([("owner".to_owned(), json!("a"))].into_iter().collect()),
        };
        let lines = session_opened_lines(&record).expect("lines");
        assert_eq!(
            lines.plain,
            "{\"type\":\"session_opened\",\"sessionId\":\"s1\",\"durableSessionId\":\"d1\",\"kind\":\"worker\",\"cwd\":\"/w\"}\n"
        );
        let with: serde_json::Value = serde_json::from_str(&lines.with_context).expect("json");
        assert_eq!(with["context"], json!({"owner":"a"}));
    }
}
