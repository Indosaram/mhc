use maho_ai::{model::Model,types::InputModality};
pub fn tool_activation(enabled:bool,model:Option<&Model>,vision_model_available:bool,active:&[String])->Option<Vec<String>> {
    let should_be_active=enabled && model.is_some_and(|model|!model.input.contains(&InputModality::Image)) && vision_model_available;
    let is_active=active.iter().any(|name|name=="look_at");
    if should_be_active && !is_active { let mut next=active.to_vec(); next.push("look_at".into()); Some(next) }
    else if !should_be_active && is_active { Some(active.iter().filter(|name|name.as_str()!="look_at").cloned().collect()) } else { None }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn absent_model_and_disabled_setting_remove_tool() { let active=["read".into(),"look_at".into()]; assert_eq!(tool_activation(true,None,true,&active),Some(vec!["read".into()])); assert_eq!(tool_activation(false,None,true,&active),Some(vec!["read".into()])); assert_eq!(tool_activation(true,None,true,&["read".into()]),None); }
}
