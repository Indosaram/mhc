//! Port of `omo-senpi/src/extension/index.ts`, `bundled-index.ts` and `omo-task.ts` at pin
//! `77f3067f1`: the shipped extension entry.
//!
//! Upstream always supplies both the `task` component (an argument to `createOmoSenpiComponents`)
//! and `memory`; there is no optional entry, and `compose.ts` registers a disabled flag per entry.
//! The required-slot entry functions here mirror that, and the `try_*` variants return
//! [`MissingComponents`] rather than silently composing a short list.

use crate::component_list::{MissingComponents, OmoComponentOptions, omo_components, try_omo_components};
use crate::compose::{OmoExtension, OmoSenpiComponent, compose_omo_extension_with_options};
use crate::provisioning::ProvisioningOptions;
use crate::runtime::OmoRuntimeOptions;

pub fn omo_senpi_extension(
    task: OmoSenpiComponent,
    memory: OmoSenpiComponent,
    components: &OmoComponentOptions,
    runtime: OmoRuntimeOptions,
    provisioning: ProvisioningOptions,
) -> OmoExtension {
    compose_omo_extension_with_options(omo_components(task, memory, components), runtime, provisioning)
}

pub fn try_omo_senpi_extension(
    task: Option<OmoSenpiComponent>,
    memory: Option<OmoSenpiComponent>,
    components: &OmoComponentOptions,
    runtime: OmoRuntimeOptions,
    provisioning: ProvisioningOptions,
) -> Result<OmoExtension, MissingComponents> {
    Ok(compose_omo_extension_with_options(try_omo_components(task, memory, components)?, runtime, provisioning))
}

pub fn bundled_omo_senpi_extension(
    task: OmoSenpiComponent,
    memory: OmoSenpiComponent,
    components: &OmoComponentOptions,
    runtime: OmoRuntimeOptions,
    provisioning: ProvisioningOptions,
) -> OmoExtension {
    omo_senpi_extension(task, memory, components, runtime, provisioning)
}

pub fn omo_task_component(task: OmoSenpiComponent) -> OmoSenpiComponent {
    task
}
