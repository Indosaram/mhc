use std::sync::{Arc, Mutex};
use maho_ext_api::*;
use maho_omo_native_badge::*;

#[derive(Default)]
struct Ui(Mutex<Vec<(String, Option<String>)>>);
impl ExtensionUi for Ui {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, key: &str, text: Option<&str>) { self.0.lock().expect("test lock").push((key.into(),text.map(str::to_owned))); }
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("unavailable")) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn api() -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("badge", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default())
}
#[test] fn publishes_badge() { let ui=Ui::default(); NativeBadgeStatus.publish(Some(&ui)); assert_eq!(*ui.0.lock().expect("test lock"), vec![(NATIVE_BADGE_STATUS_KEY.into(),Some(NATIVE_BADGE_TEXT.into()))]); }
#[test] fn repeats_same_key() { let ui=Ui::default(); NativeBadgeStatus.publish(Some(&ui)); NativeBadgeStatus.publish(Some(&ui)); let calls=ui.0.lock().expect("test lock"); assert_eq!(calls.len(),2); assert_eq!(calls[0],calls[1]); }
#[test] fn missing_ui_is_silent() { NativeBadgeStatus.publish(None); NativeBadgeStatus.clear(None); }
#[test] fn key_sorts_first() { for key in ["ulw-loop","memory","omo-task"] { assert!(NATIVE_BADGE_STATUS_KEY < key); } }
#[test] fn component_name() { assert_eq!(NativeBadgeComponent::NAME,"native-badge"); }
#[test] fn registers_session_start() { let mut api=api(); NativeBadgeComponent.register(&mut api); assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(),1); }
#[test] fn registers_settled() { let mut api=api(); NativeBadgeComponent.register(&mut api); assert_eq!(api.registered.handlers[&EventKind::AgentSettled].len(),1); }
#[test] fn registration_has_only_two_handlers() { let mut api=api(); NativeBadgeComponent.register(&mut api); assert_eq!(api.registered.handlers.len(),2); }
#[test] fn clears_badge() { let ui=Arc::new(Ui::default()); NativeBadgeStatus.clear(Some(ui.as_ref())); assert_eq!(*ui.0.lock().expect("test lock"),vec![(NATIVE_BADGE_STATUS_KEY.into(),None)]); }
