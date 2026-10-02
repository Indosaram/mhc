pub type NetworkRequirementsDomains1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::network_domain_permission::NetworkDomainPermission>>;

pub type NetworkRequirementsUnixSockets2 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::network_unix_socket_permission::NetworkUnixSocketPermission>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NetworkRequirements {
    #[serde(rename = "enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub enabled: Option<bool>,
    #[serde(rename = "httpPort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub http_port: Option<f64>,
    #[serde(rename = "socksPort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub socks_port: Option<f64>,
    #[serde(rename = "allowUpstreamProxy", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_upstream_proxy: Option<bool>,
    #[serde(rename = "dangerouslyAllowNonLoopbackProxy", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub dangerously_allow_non_loopback_proxy: Option<bool>,
    #[serde(rename = "dangerouslyAllowAllUnixSockets", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub dangerously_allow_all_unix_sockets: Option<bool>,
    #[serde(rename = "domains", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub domains: Option<NetworkRequirementsDomains1>,
    #[serde(rename = "managedAllowedDomainsOnly", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub managed_allowed_domains_only: Option<bool>,
    #[serde(rename = "allowedDomains", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(rename = "deniedDomains", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub denied_domains: Option<Vec<String>>,
    #[serde(rename = "unixSockets", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub unix_sockets: Option<NetworkRequirementsUnixSockets2>,
    #[serde(rename = "allowUnixSockets", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_unix_sockets: Option<Vec<String>>,
    #[serde(rename = "allowLocalBinding", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_local_binding: Option<bool>,
}
