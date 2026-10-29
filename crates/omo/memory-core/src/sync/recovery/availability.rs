use super::ports::RecoveryBridge;
pub const APPROVAL_PATH_MESSAGE: &str =
    "Recovery on this device needs approval from another of your devices.";
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryAvailability {
    Passkey,
    ApprovalOnly {
        reason: &'static str,
        message: &'static str,
    },
}
pub fn recovery_availability(bridge: Option<&dyn RecoveryBridge>) -> RecoveryAvailability {
    match bridge {
        None => RecoveryAvailability::ApprovalOnly {
            reason: "no-native-bridge",
            message: APPROVAL_PATH_MESSAGE,
        },
        Some(bridge) if !bridge.prf_supported() => RecoveryAvailability::ApprovalOnly {
            reason: "no-prf",
            message: APPROVAL_PATH_MESSAGE,
        },
        Some(_) => RecoveryAvailability::Passkey,
    }
}
