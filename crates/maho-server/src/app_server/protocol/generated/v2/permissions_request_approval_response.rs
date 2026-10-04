#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PermissionsRequestApprovalResponse {
    #[serde(rename = "permissions")]
    pub permissions: Box<crate::app_server::protocol::generated::v2::granted_permission_profile::GrantedPermissionProfile>,
    #[serde(rename = "scope")]
    pub scope: Box<crate::app_server::protocol::generated::v2::permission_grant_scope::PermissionGrantScope>,
    #[serde(rename = "strictAutoReview", default, skip_serializing_if = "Option::is_none")]
    pub strict_auto_review: Option<bool>,
}
