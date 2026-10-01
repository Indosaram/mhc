pub const INTERNAL_SUPERVISOR_ROUTE_FLAG:&str="--internal-rpc-host-supervisor";
pub fn internal_supervisor_payload(args:&[String])->Option<&[String]>{
    if !args.iter().any(|arg|arg==INTERNAL_SUPERVISOR_ROUTE_FLAG){return None;}
    crate::host_lifecycle::find_internal_supervisor_args(args)
}
#[cfg(test)]mod tests{use super::*;#[test]fn sentinel_is_equal_and_value_cannot_dispatch(){assert_eq!(INTERNAL_SUPERVISOR_ROUTE_FLAG,crate::host_lifecycle::INTERNAL_SUPERVISOR_FLAG);assert!(internal_supervisor_payload(&["--model".into(),INTERNAL_SUPERVISOR_ROUTE_FLAG.into()]).is_none());assert_eq!(internal_supervisor_payload(&[INTERNAL_SUPERVISOR_ROUTE_FLAG.into()]),Some([].as_slice()));}}
