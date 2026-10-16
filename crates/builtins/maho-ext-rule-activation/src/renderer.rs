use std::{cell::RefCell, rc::Rc};
use maho_ext_api::{Component, EntryRenderOptions, SessionEntry, Theme};
use maho_tui::{components::box_::Box as NoticeBox, utils::wrap_text_with_ansi};
use crate::types::{Remediation, RuleActivationDetails, parse_rule_activation_details};
/// notice/box.ts `${BOLD}title${BOLD_OFF}`: the extension-facing theme exposes only fg/bg.
/// The pinned plain-theme reference renders tones as identity; only apply a tone the theme defines.
fn tone(theme: &Theme, color: &str, text: &str) -> String { if theme.colors.contains_key(color) { theme.fg(color, text) } else { text.to_owned() } }
fn bold(text: &str) -> String { format!("\u{1b}[1m{text}\u{1b}[22m") }
struct NoticeText(String);
impl Component for NoticeText { fn render(&mut self, width: usize) -> Vec<String> { wrap_text_with_ansi(&self.0, width.max(1)) } }
pub fn render_rule_activation_entry(entry: &SessionEntry, options: &EntryRenderOptions, theme: &Theme) -> Option<Box<dyn Component>> {
 let details = parse_rule_activation_details(&entry.data)?;
 let (title, why, detail) = match details {
  RuleActivationDetails::ProjectRules { target_path, rules, .. } => (
   tone(theme, "accent", &bold(&format!("● Project rules · {target_path}"))),
   tone(theme, "dim", &format!("{} {} matched and injected for this tool result.", rules.len(), if rules.len() == 1 { "instruction" } else { "instructions" })),
   rules.iter().map(|rule| format!("rule {rule}")).collect::<Vec<_>>().join("\n")),
  RuleActivationDetails::Ttsr { owner, rules, remediation } => (
   tone(theme, "accent", &bold(&format!("⚠ Stream rule · {owner}"))),
   tone(theme, "dim", match remediation { Remediation::Nudge => "Output interrupted; a corrective nudge was queued.", Remediation::ProviderError => "Corrupted generation discarded; bounded provider retry started." }),
   format!("remediation {} · observed {}", remediation.as_str(), rules.join(", "))),
 };
 let mut notice = NoticeBox::with_padding(1, 1);
 notice.add_child(Rc::new(RefCell::new(NoticeText(title))));
 notice.add_child(Rc::new(RefCell::new(NoticeText(why))));
 if options.expanded { notice.add_child(Rc::new(RefCell::new(NoticeText(tone(theme, "dim", &detail))))); }
 Some(Box::new(notice))
}
