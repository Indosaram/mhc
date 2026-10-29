use loro::LoroDoc;
pub fn new_tag(doc: &LoroDoc, salt: &str, slot: usize) -> String {
    let at = if slot == 0 {
        doc.len_ops().to_string()
    } else {
        format!("{}+{slot}", doc.len_ops())
    };
    format!(
        "{}@{at}#{}{}",
        doc.peer_id(),
        fnv1a(salt, 2166136261),
        fnv1a(salt, 424242)
    )
}
fn fnv1a(text: &str, mut hash: u32) -> String {
    for unit in text.encode_utf16() {
        hash = (hash ^ u32::from(unit)).wrapping_mul(16777619);
    }
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit(hash % 36, 36).unwrap_or('0'));
        hash /= 36;
        if hash == 0 {
            break;
        }
    }
    digits.into_iter().rev().collect()
}
