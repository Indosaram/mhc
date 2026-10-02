pub const MAX_OBJECTIVE_LENGTH:usize=4000;
#[derive(Debug,PartialEq,Eq)]
pub struct ValidatedObjective{pub objective:String,pub truncated:bool,pub full_text_file_name:Option<String>}
pub fn validate_objective(value:&str,full_text_file_name:&str)->Result<ValidatedObjective,String>{
    let objective=value.trim_matches(js_whitespace);
    if objective.is_empty(){return Err("objective must not be empty".into());}
    let points:Vec<_>=objective.chars().collect();
    if points.len()<=MAX_OBJECTIVE_LENGTH{return Ok(ValidatedObjective{objective:objective.into(),truncated:false,full_text_file_name:None});}
    let marker=truncation_marker(full_text_file_name);
    let payload_budget=MAX_OBJECTIVE_LENGTH.checked_sub(marker.chars().count()).ok_or_else(||"full objective filename exceeds objective budget".to_owned())?;
    let minimum=payload_budget.saturating_sub(200);
    let cut=(minimum..payload_budget).rev().find(|index|js_whitespace(points[*index])).unwrap_or(payload_budget);
    let payload:String=points[..cut].iter().collect();
    Ok(ValidatedObjective{objective:format!("{payload}{marker}"),truncated:true,full_text_file_name:Some(full_text_file_name.into())})
}
pub fn validate_token_budget(value:f64)->Result<u64,String>{
    if !value.is_finite() || !(0.0..=9_007_199_254_740_991.0).contains(&value) || value.fract()!=0.0{return Err("tokenBudget must be a non-negative safe integer".into());}
    format!("{value:.0}").parse().map_err(|_|"tokenBudget must be a non-negative safe integer".into())
}
pub fn resolve_token_budget(current:Option<u64>,update:Option<Option<f64>>)->Result<Option<u64>,String>{match update{None=>Ok(current),Some(None)=>Ok(None),Some(Some(value))=>validate_token_budget(value).map(Some)}}
pub fn truncation_marker(full_text_file_name:&str)->String{format!("… [truncated; full objective: {full_text_file_name}]")}
pub fn objective_truncation_notice(full_text_file_name:&str)->String{format!("Objective was truncated; full objective saved to {full_text_file_name}.")}
pub fn js_whitespace(ch:char)->bool{matches!(ch,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')}
