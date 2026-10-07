use crate::internal::validate::{Node, any, record};

/// The `gateway` section belongs to a separately installed package, which validates it. omo only
/// accepts the key as an object, so an omo.json that carries the section loads without an
/// unknown-key diagnostic; omo itself never reads it.
pub fn omo_gateway_section_schema() -> Node {
    record(any())
}
