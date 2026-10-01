use std::{cell::RefCell, rc::Rc};
use maho_ext_api::{Component, EntryRenderOptions, SessionEntry, Theme};
use maho_tui::{components::box_::Box as NoticeBox, utils::wrap_text_with_ansi};
use crate::types::{Remediation, RuleActivationDetails, parse_rule_activation_details};
struct NoticeText(String);
impl Component for NoticeText { fn render(&mut self, width: usize) -> Vec<String> { wrap_text_with_ansi(&self.0, width.max(1)) } }
pub fn render_rule_activation_entry(entry: &SessionEntry, options: &EntryRenderOptions, _theme: &Theme) -> Option<Box<dyn Component>> {
 let details = parse_rule_activation_details(&entry.data)?;
 let (title, why, detail) = match details {
  RuleActivationDetails::ProjectRules { target_path, rules, .. } => (
   format!("● Project rules · {target_path}"),
   format!("{} {} matched and injected for this tool result.", rules.len(), if rules.len() == 1 { "instruction" } else { "instructions" }),
   rules.iter().map(|rule| format!("rule {rule}")).collect::<Vec<_>>().join("\n")),
  RuleActivationDetails::Ttsr { owner, rules, remediation } => (
   format!("⚠ Stream rule · {owner}"),
   match remediation { Remediation::Nudge => "Output interrupted; a corrective nudge was queued.", Remediation::ProviderError => "Corrupted generation discarded; bounded provider retry started." }.into(),
   format!("remediation {} · observed {}", remediation.as_str(), rules.join(", "))),
 };
 let mut notice = NoticeBox::with_padding(1, 1);
 notice.add_child(Rc::new(RefCell::new(NoticeText(format!("\x1b[1m{title}\x1b[22m")))));
 notice.add_child(Rc::new(RefCell::new(NoticeText(why))));
 if options.expanded { notice.add_child(Rc::new(RefCell::new(NoticeText(detail)))); }
 Some(Box::new(notice))
}
