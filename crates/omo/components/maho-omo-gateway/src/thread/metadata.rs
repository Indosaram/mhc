//! Metadata for the thread tool family (`thread/metadata.ts`).

pub const THREAD_TOOL_EXPOSURE: &str = "eval";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadToolEntry {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub exposure: &'static str,
}

pub const THREAD_TOOL_METADATA: &[ThreadToolEntry] = &[
    ThreadToolEntry {
        name: "thread_create",
        label: "Create session",
        description: "Starts a fresh agent session that runs alongside this one, for when the user wants a second session working in parallel; to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_list",
        label: "List sessions",
        description: "Shows every addressable session on this device and the account's other devices with its id, name, status and device, plus the account's devices, for when the user needs a valid target address before acting on a session; to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_read",
        label: "Read session",
        description: "Returns the transcript and latest output of another session, for when the user asks what another session said or produced; to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_send",
        label: "Send message to session",
        description: "Delivers a message into another session that is already running, for when the user wants to talk to that session mid-task; its reply comes back through thread_read, and a bound chat thread's replies through thread_report and thread_answer (in an eval cell: tool.thread_read(), tool.thread_report(), tool.thread_answer()); to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_interrupt",
        label: "Interrupt session turn",
        description: "Stops the turn running in another session, and the background jobs that turn started, for when the user wants to halt a session that is going down the wrong path; the result's background lists what was stopped; to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_close",
        label: "Close session",
        description: "Closes a session. Omit thread or pass self to close this session. thread accepts the thread value of a thread_list row (tool.thread_list() in an eval cell), an alias, or a unique name; the result is closed true or false plus a reason. A thread with messages it has not read is refused, naming them; pass force: true to close it anyway (dropped_deliveries lists them); to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_handoff",
        label: "Hand off session",
        description: "Moves the current request to an old session that has the context, for when the user says to hand this off or reopen that earlier conversation; to spawn a child task instead, use task.",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_rename",
        label: "Rename session",
        description: "Renames a session's label, for when the user asks to call a session something clearer. Omit thread or pass self to rename this session. thread accepts the thread value of a thread_list row (tool.thread_list() in an eval cell), an alias, or a unique name; a miss names the thread value to pass",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_set_model",
        label: "Switch session model",
        description: "Moves a session onto a different model, for when the user asks to switch that session to another model or a cheaper one",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_set_reasoning",
        label: "Set session reasoning level",
        description: "Sets how much reasoning a session applies, for when the user asks to raise or lower the thinking effort of a session",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_archive",
        label: "Archive session",
        description: "Archives or unarchives a session so the session list hides or shows it again, for when the user is finished with a session but wants to keep it",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_report",
        label: "Report to chat thread",
        description: "Posts a milestone, result, question or completion from this session to its bound chat thread, for when the user wants progress relayed to that chat; replies flow back through thread_read, thread_report and thread_answer (in an eval cell: tool.thread_read(), tool.thread_report(), tool.thread_answer())",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_outbox",
        label: "Read chat outbox",
        description: "Returns the reports waiting for a bound chat thread with a cursor, for when a connector collects what sessions posted",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_outbox_ack",
        label: "Acknowledge chat outbox",
        description: "Marks outbox rows up to a cursor as posted to the chat, for when a connector confirms delivery",
        exposure: THREAD_TOOL_EXPOSURE,
    },
    ThreadToolEntry {
        name: "thread_answer",
        label: "Answer relayed question",
        description: "Resolves a question a session relayed to a chat thread with the answer that arrived there, for when a connector hands back a chat user's reply",
        exposure: THREAD_TOOL_EXPOSURE,
    },
];

pub const THREAD_FAMILY_PROMPT_GUIDELINES: &str = "Thread tools address peer sessions, never child tasks; while an eval tool exists they run only inside an eval cell, as tool.thread_<verb>(): call tool.thread_list() first and pass a row's thread value (every result names a session as thread, the exact string to pass back), an alias, or a unique name; tool.thread_rename() and tool.thread_close() take this session when thread is omitted or self; an ambiguous name returns candidates instead of delivering; leave all_scope unset to stay inside this workspace and set it only when the caller explicitly asks for sessions in every workspace; when you mention a thread to the user, paste the link its result carries instead of writing its id or inventing a link.";

pub fn metadata_for(name: &str) -> Option<&'static ThreadToolEntry> {
    THREAD_TOOL_METADATA.iter().find(|entry| entry.name == name)
}
