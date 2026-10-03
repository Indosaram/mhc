use std::collections::HashSet;
use maho_ext_api::{CustomMessage, DeliverAs, ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionMode, SendMessageOptions, ToolContent};
use crate::tool::detached_cell_notification::EvalDetachedCellNotification;

pub const EVAL_NOTIFICATION_CUSTOM_TYPE: &str = "senpi-codemode:notification";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalNotifyMode { Wake, NextTurn, Off }

#[derive(Default)]
pub struct EvalNotifier { notified: HashSet<String> }

impl EvalNotifier {
    pub fn reset(&mut self) { self.notified.clear(); }

    pub fn notification_message(&mut self, cells: &[EvalDetachedCellNotification], mode: EvalNotifyMode, context: Option<(ExtensionMode, bool)>) -> Option<(CustomMessage, SendMessageOptions)> {
        if mode == EvalNotifyMode::Off { return None; }
        let (context_mode, has_model) = context?;
        if matches!(context_mode, ExtensionMode::Print | ExtensionMode::Json) || !has_model { return None; }
        let pending: Vec<_> = cells.iter().filter(|cell| !self.notified.contains(&cell.cell_id)).collect();
        if pending.is_empty() { return None; }
        for cell in &pending { self.notified.insert(cell.cell_id.clone()); }
        Some((CustomMessage {
            custom_type: EVAL_NOTIFICATION_CUSTOM_TYPE.into(),
            content: vec![ToolContent::text(pending.iter().map(|cell| cell.content.as_str()).collect::<Vec<_>>().join("\n\n"))],
            display: false,
            details: None,
        }, SendMessageOptions { trigger_turn: true, deliver_as: Some(if mode == EvalNotifyMode::Wake { DeliverAs::Steer } else { DeliverAs::FollowUp }) }))
    }

    pub fn notify(&mut self, api: &ExtensionApi, context: Option<&ExtensionContext>, mode: EvalNotifyMode, cells: &[EvalDetachedCellNotification]) -> Result<(), ExtensionFailure> {
        if let Some((message, options)) = self.notification_message(cells, mode, context.map(|ctx| (ctx.mode, ctx.model.is_some()))) {
            api.send_message(message, options)?;
        }
        Ok(())
    }
}
