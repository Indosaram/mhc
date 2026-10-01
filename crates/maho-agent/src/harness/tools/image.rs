const PNG_SIGNATURE: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if buffer.starts_with(&[0xff, 0xd8, 0xff]) {
        return (buffer.get(3) != Some(&0xf7)).then_some("image/jpeg");
    }
    if buffer.starts_with(PNG_SIGNATURE) {
        return (is_png(buffer) && !is_animated_png(buffer)).then_some("image/png");
    }
    if buffer.starts_with(b"GIF") {
        return Some("image/gif");
    }
    if buffer.starts_with(b"RIFF") && starts_with_ascii(buffer, 8, b"WEBP") {
        return Some("image/webp");
    }
    if buffer.starts_with(b"BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        output.push(char::from(ALPHABET[usize::from(first >> 2)]));
        output.push(char::from(
            ALPHABET[usize::from(((first & 3) << 4) | (second.unwrap_or(0) >> 4))],
        ));
        output.push(second.map_or('=', |second| {
            char::from(ALPHABET[usize::from(((second & 15) << 2) | (third.unwrap_or(0) >> 6))])
        }));
        output.push(third.map_or('=', |third| char::from(ALPHABET[usize::from(third & 63)])));
    }
    output
}
fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16 && read_u32_be(buffer, 8) == 13 && starts_with_ascii(buffer, 12, b"IHDR")
}
fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = 8;
    while offset + 8 <= buffer.len() {
        let length = u64::from(read_u32_be(buffer, offset));
        if starts_with_ascii(buffer, offset + 4, b"acTL") {
            return true;
        }
        if starts_with_ascii(buffer, offset + 4, b"IDAT") {
            return false;
        }
        let next = u64::try_from(offset)
            .unwrap_or(u64::MAX)
            .saturating_add(12)
            .saturating_add(length);
        let Ok(next) = usize::try_from(next) else {
            return false;
        };
        if next <= offset || next > buffer.len() {
            return false;
        }
        offset = next;
    }
    false
}
fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let size = read_u32_le(buffer, 2);
    let pixel = read_u32_le(buffer, 10);
    let dib = read_u32_le(buffer, 14);
    if size != 0 && size < 26
        || u64::from(pixel) < 14 + u64::from(dib)
        || size != 0 && pixel >= size
    {
        return false;
    }
    let (planes, bits) = if dib == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };
    planes == 1 && [1, 4, 8, 16, 24, 32].contains(&bits)
}
fn read_u16_le(buffer: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(std::array::from_fn(|i| {
        buffer.get(offset + i).copied().unwrap_or(0)
    }))
}
fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(std::array::from_fn(|i| {
        buffer.get(offset + i).copied().unwrap_or(0)
    }))
}
fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|i| {
        buffer.get(offset + i).copied().unwrap_or(0)
    }))
}
fn starts_with_ascii(buffer: &[u8], offset: usize, text: &[u8]) -> bool {
    buffer.get(offset..offset + text.len()) == Some(text)
}
