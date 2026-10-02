#[derive(Debug, PartialEq, Eq)]
pub struct DisplayNameCommand {
    pub account_id: String,
    pub display_name: Option<String>,
}

pub fn parse_display_name_command(
    raw: &str,
) -> Result<Option<DisplayNameCommand>, &'static str> {
    let raw = raw.trim();
    let mut words = raw.split_whitespace();
    let action = words.next().unwrap_or("");
    if action != "rename" && action != "clear-name" {
        return Ok(None);
    }
    let name = words
        .next()
        .ok_or("Usage: rename <id> <display name...> or clear-name <id>")?;
    if action == "clear-name" {
        if words.next().is_some() {
            return Err("Usage: rename <id> <display name...> or clear-name <id>");
        }
        return Ok(Some(DisplayNameCommand {
            account_id: name.into(),
            display_name: None,
        }));
    }
    let after_action = raw[action.len()..].trim_start();
    let display_name = after_action[name.len()..].trim_start().to_owned();
    Ok(Some(DisplayNameCommand {
        account_id: name.into(),
        display_name: Some(display_name),
    }))
}
