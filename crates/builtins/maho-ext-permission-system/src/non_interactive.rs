use crate::{
    events::PermissionEventEmitter,
    types::{DENIAL_IS_FINAL, Reply, ReplyInput, Request},
};

/// Answers, with no UI to ask (print mode, unbound SDK), a request the permission service is still
/// asking about. The service has already applied every rule and found at least one pattern it must
/// ask about, so the answer is always a refusal with a reason, never an approval; a configured allow
/// or deny for the other patterns does not change that. `preset_bound` is set when the `auto`
/// preset is active.
pub fn handle_no_ui(
    request: &Request,
    preset_bound: bool,
    emitter: &PermissionEventEmitter,
) -> Result<ReplyInput, serde_json::Error> {
    emitter.emit_asked(request)?;
    let patterns = request.patterns.join(", ");
    let permission = &request.permission;
    // mhc divergence (docs/divergences.md SD-1): a headless external path names the guarded-preset
    // escape hatches instead of the generic flag hint. Still a refusal.
    let message = if permission == "external_directory" && !preset_bound {
        format!(
            "Permission required to access {patterns} outside the working directory. Allow it with --permission external_directory=allow, set permissionPreset to \"full-access\", or add a permission rule."
        )
    } else if preset_bound {
        format!(
            "Permission required for {permission} ({patterns}), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} Under the auto preset, allow rules do not widen what it approves; the user can run it interactively or choose another preset."
        )
    } else {
        format!(
            "Permission required for {permission} ({patterns}), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission {permission}=allow."
        )
    };
    Ok(ReplyInput {
        request_id: request.id.clone(),
        reply: Reply::Reject,
        always: None,
        message: Some(message),
    })
}
