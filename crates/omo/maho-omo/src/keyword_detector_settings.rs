use maho_ultrawork::{UltraworkExtension, keyword_activation_allowed};
use omo_config_core::{LoadOmoConfigOptions, OmoConfigEnv, load_omo_config};
use serde_json::Value;

use crate::compose::OmoSenpiComponent;

pub(crate) fn component(env: OmoConfigEnv) -> OmoSenpiComponent {
    let legacy_child = env.contains_key("SENPI_TASK_MEMBER")
        || env
            .get("OMO_SENPI_TASK_RPC_CHILD")
            .is_some_and(|value| value == "1");
    OmoSenpiComponent::new(
        "ultrawork",
        Box::new(UltraworkExtension::default().with_keyword_policy(
            move |cwd| {
                let loaded = load_omo_config(&LoadOmoConfigOptions {
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    env: Some(env.clone()),
                    harness: Some("native".into()),
                    ..Default::default()
                });
                keyword_activation_allowed(&Value::Object(loaded.config))
            },
            legacy_child,
        )),
    )
}
