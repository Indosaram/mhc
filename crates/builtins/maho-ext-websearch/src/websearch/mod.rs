pub mod provider_endpoints;
pub mod providers;
pub mod types;
pub mod search;
pub mod config;
pub mod native;
pub mod tool;
pub mod renderers;
pub mod hosted_routes;
pub mod html_document;
pub mod route_attempts;
pub mod search_model;
pub mod session_login_entries;
/// Engine providers ported by the ledger lane. `providers/mod.rs` (and the `SearchProvider`
/// dispatch in it) belongs to senpi-builtins-b, so these compile here until that lane lists them.
#[path = "providers"]
pub mod engines {
    pub mod ecosia;
    pub mod exa_mcp;
    pub mod google_html;
    pub mod kagi;
    pub mod keenable;
    pub mod mojeek;
    pub mod response_stream;
    pub mod searxng;
    pub mod serpdive;
    pub mod startpage;
}
