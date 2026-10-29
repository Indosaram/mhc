pub fn missing_task_capabilities(send_message: bool, register_message_renderer: bool) -> Vec<&'static str> {
    [("sendMessage", send_message), ("registerMessageRenderer", register_message_renderer)].into_iter().filter_map(|(name, available)| (!available).then_some(name)).collect()
}
