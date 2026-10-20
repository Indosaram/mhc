use super::{
    magic_cells::{MagicCell, parse_magic_cell},
    types::HostCellExecutor,
};
use crate::environments::js_environments::JsEnvironments;
use serde_json::json;
use std::sync::Arc;
pub fn plan_magic_cell(
    code: &str,
    environments: Option<Arc<JsEnvironments>>,
) -> Option<HostCellExecutor> {
    let magic = match parse_magic_cell(code) {
        Ok(None) | Ok(Some(MagicCell::Load(_))) => return None,
        Ok(Some(magic)) => magic,
        Err(error) => return Some(refused_entry(error)),
    };
    let Some(environments) = environments else {
        return Some(refused_entry(
            "environment_installer_unavailable: this session has no JavaScript package environment"
                .into(),
        ));
    };
    Some(Box::new(move |context| {
        Box::pin(async move {
            match magic {
                MagicCell::Environment(mode) => match environments.set_mode(mode).await {
                    Ok(_) => {
                        json!({"ok":true,"valueRepr":format!("environment: {} ({})",mode.name(),if mode.name()=="project" {"the session directory"} else {"this session's managed revisions"})})
                    }
                    Err(error) => json!({"ok":false,"error":{"message":error.to_string()}}),
                },
                MagicCell::JsAdd { installer, args } => {
                    (context.emit)(
                        &json!({"type":"status","event":{"op":crate::bridge::reserved::TIMEOUT_PAUSE_OP}}),
                    );
                    let emit = context.emit.clone();
                    let receipt = environments
                        .install(
                            &args,
                            &context.signal,
                            Some(Arc::new(move |stream, data| {
                                emit(&json!({"type":"text","stream":stream,"data":data}))
                            })),
                            Some(installer),
                        )
                        .await;
                    (context.emit)(
                        &json!({"type":"status","event":{"op":crate::bridge::reserved::TIMEOUT_RESUME_OP}}),
                    );
                    match receipt {
                        Ok(receipt) => {
                            let added = if receipt.added.is_empty() {
                                "nothing new".into()
                            } else {
                                receipt.added.join(", ")
                            };
                            let where_ = receipt.revision.map_or_else(
                                || receipt.mode.name().to_owned(),
                                |revision| format!("{} (revision {revision})", receipt.mode.name()),
                            );
                            let shadow = if receipt.shadowed.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "; environment_resolution_conflict: {} still {} from the project's node_modules first",
                                    receipt.shadowed.join(", "),
                                    if receipt.shadowed.len() == 1 {
                                        "resolves"
                                    } else {
                                        "resolve"
                                    }
                                )
                            };
                            json!({"ok":true,"valueRepr":format!("added {added} with {} into {where_}{shadow}; already-imported modules stay cached until reset",receipt.installer)})
                        }
                        Err(error) => json!({"ok":false,"error":{"message":error.to_string()}}),
                    }
                }
                MagicCell::Load(_) => unreachable!("load is not a host install"),
            }
        })
    }))
}
pub fn refused_entry(message: String) -> HostCellExecutor {
    Box::new(move |_| Box::pin(async move { json!({"ok":false,"error":{"message":message}}) }))
}
