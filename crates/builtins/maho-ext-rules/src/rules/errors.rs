use std::fmt;

#[derive(Debug)]
pub struct UnsupportedRuleSourceError(pub String);
impl fmt::Display for UnsupportedRuleSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { formatter.write_str(&self.0) }
}
impl std::error::Error for UnsupportedRuleSourceError {}

#[derive(Debug)]
pub struct RuleFrontmatterParseError(pub String);
impl fmt::Display for RuleFrontmatterParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { formatter.write_str(&self.0) }
}
impl std::error::Error for RuleFrontmatterParseError {}
