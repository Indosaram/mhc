use crate::runners::in_process::shared_tool_filter::is_task_or_team_family_tool;
pub const KERNEL_TOOL_NAME_MAX_LENGTH:usize=64;
pub const RESERVED_KERNEL_TOOL_ALIASES:&[&str]= &["__agent__","__output__","__schema__","eval","monitor"];
pub fn sanitize_kernel_tool_name_part(name:&str)->String { name.chars().map(|c|if c.is_ascii_alphanumeric()||c=='_'||c=='-'{c}else{'_'}).collect() }
pub fn normalize_kernel_tool_name(name:&str)->String { let v=sanitize_kernel_tool_name_part(name); if v.len()<=64{return v} let p=31; format!("{}...{}",&v[..p],&v[v.len()-30..]) }
pub fn kernel_tool_key(name:&str)->String { normalize_kernel_tool_name(name).replace('-',"_") }
pub fn is_reserved_kernel_tool_name(name:&str)->bool { let n=kernel_tool_key(name); RESERVED_KERNEL_TOOL_ALIASES.iter().any(|x|kernel_tool_key(x)==n)||is_task_or_team_family_tool(&normalize_kernel_tool_name(name))||n=="workpool" }
