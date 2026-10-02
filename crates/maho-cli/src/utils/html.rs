pub struct DecodedHtmlEntity { pub text: String, pub length: usize }
pub fn decode_html_entity(entity: &str) -> Option<String> {
    let named = match entity { "amp" => Some("&"), "lt" => Some("<"), "gt" => Some(">"), "quot" => Some("\""), "apos" => Some("'"), _ => None }; if let Some(value) = named { return Some(value.to_owned()); }
    let (digits, radix) = if let Some(value) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) { (value, 16) } else { (entity.strip_prefix('#')?, 10) };
    let digits = digits.trim_start(); let (negative, digits) = if let Some(value) = digits.strip_prefix('-') { (true, value) } else { (false, digits.strip_prefix('+').unwrap_or(digits)) };
    let length = digits.chars().take_while(|c| c.is_digit(radix)).count(); if length == 0 { return None; }
    let code = u32::from_str_radix(&digits[..length], radix).ok()?; if negative && code != 0 { return None; } char::from_u32(code).map(|c| c.to_string())
}
pub fn decode_html_entity_at(html: &str, index: usize) -> Option<DecodedHtmlEntity> {
    let byte = if index == 0 { 0 } else { html.char_indices().scan(0usize, |count, (byte, c)| { let here = *count; *count += c.len_utf16(); Some((here, byte)) }).find(|(offset, _)| *offset == index)?.1 };
    let tail = html.get(byte + 1..)?; let semicolon = tail.find(';')?; let entity = &tail[..semicolon]; let length = entity.encode_utf16().count() + 2; if length - 1 > 16 { return None; } Some(DecodedHtmlEntity { text: decode_html_entity(entity)?, length })
}
