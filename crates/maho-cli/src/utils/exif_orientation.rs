pub fn get_exif_orientation(bytes: &[u8]) -> u16 {
    let mut tiff = None;
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut offset = 2usize;
        while offset + 1 < bytes.len() {
            if bytes[offset] != 0xff { break; } let marker = bytes[offset + 1]; if marker == 0xff { offset += 1; continue; }
            if marker == 0xe1 && bytes.get(offset + 4..offset + 10) == Some(b"Exif\0\0") { tiff = Some(offset + 10); break; }
            let Some(length) = bytes.get(offset + 2..offset + 4) else { break; }; offset += 2 + u16::from_be_bytes([length[0], length[1]]) as usize;
        }
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        let mut offset = 12usize;
        while offset + 8 <= bytes.len() {
            let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().expect("four bytes")) as usize; let data = offset + 8;
            if bytes.get(data..data + size).is_none() { break; }
            if &bytes[offset..offset + 4] == b"EXIF" { tiff = Some(if size >= 6 && bytes.get(data..data + 6) == Some(b"Exif\0\0") { data + 6 } else { data }); break; }
            offset = data + size + size % 2;
        }
    }
    let Some(start) = tiff else { return 1; }; if start + 8 > bytes.len() { return 1; } let little = bytes.get(start..start + 2) == Some(b"II");
    let read16 = |offset: usize| bytes.get(offset..offset + 2).map(|b| if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let raw: [u8; 4] = bytes[start + 4..start + 8].try_into().expect("four bytes"); let offset = if little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) } as usize;
    let Some(ifd) = start.checked_add(offset) else { return 1; }; let Some(count) = read16(ifd) else { return 1; };
    for index in 0..count as usize {
        let entry = ifd + 2 + index * 12;
        if entry + 12 > bytes.len() { return 1; }
        if read16(entry) == Some(0x112) { return read16(entry + 8).filter(|value| (1..=8).contains(value)).unwrap_or(1); }
    }
    1
}
pub fn apply_exif_orientation(pixels: &[u8], width: usize, height: usize, original: &[u8]) -> (Vec<u8>, usize, usize) {
    let orientation = get_exif_orientation(original); let rotated = orientation >= 5;
    let (out_width, out_height) = if rotated { (height, width) } else { (width, height) }; let mut out = vec![0; pixels.len()];
    for y in 0..height { for x in 0..width { let (dx, dy) = match orientation { 2 => (width - 1 - x, y), 3 => (width - 1 - x, height - 1 - y), 4 => (x, height - 1 - y), 5 => (y, x), 6 => (height - 1 - y, x), 7 => (height - 1 - y, width - 1 - x), 8 => (y, width - 1 - x), _ => (x, y) }; let source = (y * width + x) * 4; let target = (dy * out_width + dx) * 4; out[target..target + 4].copy_from_slice(&pixels[source..source + 4]); } } (out, out_width, out_height)
}
