//! WHO holds an endpoint an ensure refused to touch (senpi `host-holder.ts`, omo-desktop-app#3561).
//!
//! A `foreign_writer` refusal is final: the registered generation was written by another process, it still
//! accepts at the public path, and it did not answer as a host. The generation's own record names it; this
//! is that record as the refusal reports it. A leaf on purpose: `host_decision` words refusals with it.
use serde::Serialize;
/// The generation a refusal left standing, as its registration records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostHolder {
    /// The process serving the endpoint (the generation's supervisor).
    pub pid: u32,
    pub instance_id: String,
    pub generation: u64,
    pub process_start_time: Option<String>,
    /// The engine build the generation recorded at start; `None` for a record that predates the field.
    pub engine_version: Option<String>,
    /// The launch profile the generation was started with; `None` when it recorded none.
    pub launch_profile_id: Option<String>,
    /// The process that registered the generation (the client whose ensure started it).
    pub writer_pid: Option<u32>,
}
/// The holder in one clause: `pid 4242, engine 2026.10.16, launch profile cli-profile`.
pub fn describe_host_holder(holder: &HostHolder) -> String {
    let engine = holder.engine_version.as_deref().map_or_else(
        || "an unrecorded engine".to_owned(),
        |version| format!("engine {version}"),
    );
    let profile = holder
        .launch_profile_id
        .as_deref()
        .map_or_else(String::new, |id| format!(", launch profile {id}"));
    let writer = holder
        .writer_pid
        .filter(|writer| *writer != holder.pid)
        .map_or_else(String::new, |writer| format!(", started by pid {writer}"));
    format!("pid {}, {engine}{profile}{writer}", holder.pid)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn holder() -> HostHolder {
        HostHolder {
            pid: 4242,
            instance_id: "i".into(),
            generation: 1,
            process_start_time: None,
            engine_version: Some("2026.10.16".into()),
            launch_profile_id: Some("cli-profile".into()),
            writer_pid: Some(4242),
        }
    }
    #[test]
    fn describes_pid_engine_profile_and_a_distinct_writer() {
        let mut holder = holder();
        assert_eq!(
            describe_host_holder(&holder),
            "pid 4242, engine 2026.10.16, launch profile cli-profile"
        );
        holder.writer_pid = Some(7);
        holder.engine_version = None;
        holder.launch_profile_id = None;
        assert_eq!(
            describe_host_holder(&holder),
            "pid 4242, an unrecorded engine, started by pid 7"
        );
    }
    #[test]
    fn serializes_with_camel_case_keys() {
        let value = serde_json::to_value(holder()).unwrap();
        assert_eq!(value["instanceId"], "i");
        assert_eq!(value["writerPid"], 4242);
        assert!(value["processStartTime"].is_null());
    }
}
