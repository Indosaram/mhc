#[derive(Default)]
pub struct ReloadVetoDeferral { notified_reason: Option<String> }
impl ReloadVetoDeferral {
 pub fn defer(&mut self, reason: Option<&str>) -> Option<String> {
  let effective = reason.unwrap_or("an extension blocked the reload");
  if self.notified_reason.as_deref() == Some(effective) { return None; }
  self.notified_reason = Some(effective.into());
  Some(format!("Hot-reload deferred: {effective}"))
 }
 pub fn reset(&mut self) { self.notified_reason = None; }
}
