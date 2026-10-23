//! Port of senpi `packages/tui/src/kitty-graphics-probe.ts`.
//!
//! Kitty graphics support probe for multiplexer panes whose environment says nothing about image
//! support (herdr). A 1x1 direct-transmission query (`a=q`) is written to the terminal; a terminal
//! implementing the Kitty graphics protocol replies with an APC response naming the query's image
//! id. Silence, an error payload, or a reply for another id all mean "no graphics".

pub const KITTY_GRAPHICS_PROBE_TIMEOUT_MS: u64 = 500;

/// `AAAA` is base64 for three zero bytes: one 1x1 pixel.
pub fn build_kitty_graphics_query(image_id: u32) -> String {
    format!("\x1b_Gi={image_id},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyGraphicsProbeOutcome {
    Ok,
    Err,
}

/// Classifies one APC chunk as the answer to our query. Returns `None` when the chunk is not a
/// Kitty graphics response for `image_id`, so unrelated APC traffic is left untouched.
pub fn parse_kitty_graphics_probe_response(
    data: &str,
    image_id: u32,
) -> Option<KittyGraphicsProbeOutcome> {
    let body = data.strip_prefix("\x1b_G")?.strip_suffix("\x1b\\")?;
    let (keys, message) = body.split_once(';')?;
    let matches_id = keys
        .split(',')
        .filter_map(|part| part.strip_prefix("i="))
        .any(|value| {
            !value.is_empty()
                && value.bytes().all(|b| b.is_ascii_digit())
                && value.parse::<u32>().is_ok_and(|id| id == image_id)
        });
    if !matches_id {
        return None;
    }
    Some(if message.starts_with("OK") {
        KittyGraphicsProbeOutcome::Ok
    } else {
        KittyGraphicsProbeOutcome::Err
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_query() {
        assert_eq!(
            build_kitty_graphics_query(7),
            "\x1b_Gi=7,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"
        );
    }

    #[test]
    fn parses_matching_responses_only() {
        assert_eq!(
            parse_kitty_graphics_probe_response("\x1b_Gi=7;OK\x1b\\", 7),
            Some(KittyGraphicsProbeOutcome::Ok)
        );
        assert_eq!(
            parse_kitty_graphics_probe_response("\x1b_Gi=7,q=2;ENOTSUP:x\x1b\\", 7),
            Some(KittyGraphicsProbeOutcome::Err)
        );
        assert_eq!(
            parse_kitty_graphics_probe_response("\x1b_Gi=8;OK\x1b\\", 7),
            None
        );
        assert_eq!(
            parse_kitty_graphics_probe_response("\x1b_Gi=77;OK\x1b\\", 7),
            None
        );
        assert_eq!(
            parse_kitty_graphics_probe_response("\x1b_Gi=7\x1b\\", 7),
            None
        );
        assert_eq!(parse_kitty_graphics_probe_response("\x1b_Gi=7;OK", 7), None);
        assert_eq!(parse_kitty_graphics_probe_response("hello", 7), None);
    }
}
