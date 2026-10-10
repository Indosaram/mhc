use std::collections::HashSet;
use super::contract::KernelToolInvokeScope;
use crate::runners::in_process::shared_tool_filter::is_task_or_team_family_tool;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NestedHostScopeRequest { pub child_tool_names: Option<Vec<String>>, pub tool_allowlist: Option<Vec<String>>, pub tool_denylist: Option<Vec<String>>, pub include_task_tools: bool }
pub fn is_write_capable_host_tool(name:&str)->bool { !matches!(name,"read"|"grep"|"ls"|"web_search"|"lsp_symbols"|"ask_user_question") }
fn reachable(r:&NestedHostScopeRequest)->Vec<String>{ let mut out=vec!["read","bash","edit","write","grep"].into_iter().map(String::from).collect::<Vec<_>>(); for n in r.child_tool_names.clone().unwrap_or_default(){if !out.contains(&n){out.push(n)}} out.into_iter().filter(|n|r.include_task_tools||!is_task_or_team_family_tool(n)).collect() }
pub fn child_effective_tool_names(r:&NestedHostScopeRequest)->Vec<String>{ let allow=r.tool_allowlist.as_ref().map(|x|x.iter().collect::<HashSet<_>>()); let deny=r.tool_denylist.clone().unwrap_or_default(); reachable(r).into_iter().filter(|n|!deny.contains(n)&&(allow.as_ref().is_none_or(|a|a.contains(n)))).collect() }
pub fn escalating_host_tools(r:&NestedHostScopeRequest)->Vec<String>{if (r.tool_allowlist.is_none()&&r.tool_denylist.as_ref().is_none_or(Vec::is_empty))||r.tool_denylist.as_ref().is_some_and(|items|!items.is_empty()&&items.iter().all(|name|!is_write_capable_host_tool(name))){return vec![]}let e=child_effective_tool_names(r); reachable(r).into_iter().filter(|n|is_write_capable_host_tool(n)&&!e.contains(n)).collect()}
pub fn child_invoke_scope(r:&NestedHostScopeRequest)->KernelToolInvokeScope{KernelToolInvokeScope{tools:Some(super::contract::KernelToolHostScope{allow:Some(child_effective_tool_names(r)),deny:r.tool_denylist.clone().filter(|x|!x.is_empty())})}}
pub fn nested_host_scope_message(subject:&str, escalating:&[String])->String{format!("{subject} cannot receive parent kernel tools: a parent closure's nested host calls run with the PARENT's permissions, which still reach {} after this child's own tool policy removed {}.",escalating.join(", "),if escalating.len()==1{"it"}else{"them"})}
