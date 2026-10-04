//! Re-registration contract: `ExtensionRunner::recreate` (agent_session reload) re-registers the
//! same extension objects, so every slot - including a factory-built one - must register again.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_ext_api::{Extension, ExtensionApi};
use maho_omo::{OmoExtension, OmoRuntimeOptions, OmoSenpiComponent, RecordingLogger};
use crate::support::{FakeComponent, command_names, manual_runtime_options, new_api};

fn options() -> OmoRuntimeOptions {
    manual_runtime_options(Arc::new(RecordingLogger::new()))
}

#[test]
fn a_factory_slot_registers_again_on_a_second_register() {
    let builds = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&builds);
    let slot = OmoSenpiComponent::from_factory("memory", move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::new(FakeComponent::new(|api: &mut ExtensionApi| {
            api.register_command("memory_slot", Some("slot".to_owned()), None, Arc::new(|_, _| Box::pin(async { Ok(()) })));
        }))
    });
    let extension = OmoExtension::with_options(vec![slot], options(), Default::default());

    let mut first = new_api();
    extension.register(&mut first);
    let mut second = new_api();
    extension.register(&mut second);

    assert_eq!(builds.load(Ordering::SeqCst), 2, "a fresh instance per register");
    assert_eq!(command_names(&first), vec!["memory_slot".to_owned()]);
    assert_eq!(command_names(&second), vec!["memory_slot".to_owned()]);
}

#[test]
fn a_plain_extension_slot_registers_again_on_a_second_register() {
    let slot = OmoSenpiComponent::new(
        "alpha",
        Box::new(FakeComponent::new(|api: &mut ExtensionApi| {
            api.register_command("alpha", Some("alpha".to_owned()), None, Arc::new(|_, _| Box::pin(async { Ok(()) })));
        })),
    );
    let extension = OmoExtension::with_options(vec![slot], options(), Default::default());

    let mut first = new_api();
    extension.register(&mut first);
    let mut second = new_api();
    extension.register(&mut second);

    assert_eq!(command_names(&first), vec!["alpha".to_owned()]);
    assert_eq!(command_names(&second), vec!["alpha".to_owned()]);
}

#[test]
fn a_second_register_retains_the_runtime_for_late_callbacks() {
    let slot = OmoSenpiComponent::new("alpha", Box::new(FakeComponent::new(|_| {})));
    let extension = OmoExtension::with_options(vec![slot], options(), Default::default());

    let mut api = new_api();
    extension.register(&mut api);
    let first = extension.runtime().expect("runtime after first register");
    extension.register(&mut api);
    let second = extension.runtime().expect("runtime after second register");

    // `ExtensionRunner::recreate` re-registers the same extension objects; the shared queue,
    // captured tool registry and idle coordinator must survive so late task/memory callbacks
    // still reach the live host. Only the delivery and config accessors are rebound.
    assert!(Arc::ptr_eq(&first, &second));
}
