pub const INTERNAL_SUPERVISOR_ROUTE_FLAG:&str="--internal-rpc-host-supervisor";
pub fn internal_supervisor_payload(args:&[String])->Option<&[String]>{
    if !args.iter().any(|arg|arg==INTERNAL_SUPERVISOR_ROUTE_FLAG){return None;}
    crate::host_lifecycle::find_internal_supervisor_args(args)
}
/// Runs the internal supervisor when argv selects that route, and reports whether it did
/// (senpi `dispatchInternalSupervisor`). A sentinel that parses to nothing is an internal
/// protocol fault: it fails closed rather than falling through to the public parser.
pub async fn dispatch_internal_supervisor(args:&[String])->bool{
    if !args.iter().any(|arg|arg==INTERNAL_SUPERVISOR_ROUTE_FLAG){return false;}
    let Some(payload)=crate::host_lifecycle::find_internal_supervisor_args(args)else{return false;};
    let Some(launch)=crate::host_lifecycle::parse_supervisor_args(payload)else{return false;};
    if crate::host_lifecycle::run_host_supervisor(launch).await.is_err(){std::process::exit(2);}
    true
}
#[cfg(test)]mod tests{use super::*;#[test]fn sentinel_is_equal_and_value_cannot_dispatch(){assert_eq!(INTERNAL_SUPERVISOR_ROUTE_FLAG,crate::host_lifecycle::INTERNAL_SUPERVISOR_FLAG);assert!(internal_supervisor_payload(&["--model".into(),INTERNAL_SUPERVISOR_ROUTE_FLAG.into()]).is_none());assert_eq!(internal_supervisor_payload(&[INTERNAL_SUPERVISOR_ROUTE_FLAG.into()]),Some([].as_slice()));}}
