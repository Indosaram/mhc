#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedIntervalUnit { Seconds, Minutes, Hours, Days }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveIntervalUnit { Minutes, Hours, Days }
#[derive(Clone, Debug, PartialEq)]
pub struct RequestedInterval { pub value: f64, pub unit: RequestedIntervalUnit, pub raw: String }
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveInterval { pub value: f64, pub unit: EffectiveIntervalUnit, pub human: String, pub rounded: bool, pub rounding_notice: Option<String> }
