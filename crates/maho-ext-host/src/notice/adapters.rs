use std::sync::Arc;
use maho_ext_api::{CustomMessage, EntryRenderer, MessageRenderer, SessionEntry};
use super::{box_::build_notice_box, spec::NoticeSpec};

pub fn notice_message_renderer(map: impl Fn(&CustomMessage) -> Option<NoticeSpec> + Send + Sync + 'static) -> MessageRenderer {
    Arc::new(move |message, options, theme| map(message).map(|spec| build_notice_box(spec, options.expanded, theme)))
}
pub fn notice_entry_renderer(map: impl Fn(&SessionEntry) -> Option<NoticeSpec> + Send + Sync + 'static) -> EntryRenderer {
    Arc::new(move |entry, options, theme| map(entry).map(|spec| build_notice_box(spec, options.expanded, theme)))
}
