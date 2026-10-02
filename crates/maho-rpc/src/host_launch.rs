use std::path::PathBuf;
use crate::{custom_capability::{EXTENSION_EVENTS_CAPABILITY,CUSTOM_UNSUPPORTED_CAPABILITY},host_lifecycle::INTERNAL_SUPERVISOR_FLAG};
pub const PINNED_HOST_CLIENT_CAPABILITIES:[&str;2]=[EXTENSION_EVENTS_CAPABILITY,CUSTOM_UNSUPPORTED_CAPABILITY];
#[derive(Debug,PartialEq,Eq)]pub struct HostLaunch{pub command:PathBuf,pub args:Vec<String>}
pub fn default_host_launch(supervisor_args:&[String])->std::io::Result<HostLaunch>{Ok(host_launch(std::env::current_exe()?,supervisor_args))}
pub fn host_launch(executable:PathBuf,supervisor_args:&[String])->HostLaunch{HostLaunch{command:executable,args:std::iter::once(INTERNAL_SUPERVISOR_FLAG.to_owned()).chain(supervisor_args.iter().cloned()).collect()}}
#[cfg(test)]mod tests{use super::*;#[test]fn executable_reenters_through_internal_route(){let launch=host_launch(PathBuf::from("maho"),&["--socket".into(),"s".into()]);assert_eq!(launch.command,PathBuf::from("maho"));assert_eq!(launch.args,vec![INTERNAL_SUPERVISOR_FLAG,"--socket","s"]);assert_eq!(PINNED_HOST_CLIENT_CAPABILITIES,["extension_events","custom_unsupported"]);}}
