#[derive(Debug,Clone,PartialEq,Eq)]
pub struct UnsupportedRuleSourceError(pub String);
impl std::fmt::Display for UnsupportedRuleSourceError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(&self.0)}}
impl std::error::Error for UnsupportedRuleSourceError{}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct RuleFrontmatterParseError(pub String);
impl std::fmt::Display for RuleFrontmatterParseError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(&self.0)}}
impl std::error::Error for RuleFrontmatterParseError{}
