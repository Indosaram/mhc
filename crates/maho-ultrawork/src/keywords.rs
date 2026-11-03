//! Keyword and mass-alias detection on already-masked text.

/// Occurrence counts and flags found in a masked prompt.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scan {
    pub short_form: bool,
    pub long_form: bool,
    pub mentions: usize,
    pub mass: bool,
    /// A keyword sat inside a compound identifier, extension or path.
    pub compound_rejected: bool,
}

const CONNECTORS: &str = "-_./\\";
const OPENERS: &str = "([{<\"'\u{201C}\u{2018}\u{AB},:;";
const CLOSERS: &str = ")]}>\"'\u{201D}\u{2019}\u{BB},:;!?";

enum Boundary {
    Ok,
    Compound,
    Other,
}

fn before(text: &str, at: usize) -> Boundary {
    match text[..at].chars().next_back() {
        None => Boundary::Ok,
        Some(c) if c.is_whitespace() || OPENERS.contains(c) => Boundary::Ok,
        Some(c) if CONNECTORS.contains(c) => Boundary::Compound,
        Some(_) => Boundary::Other,
    }
}

fn after(text: &str, at: usize) -> Boundary {
    let mut rest = text[at..].chars();
    match rest.next() {
        None => Boundary::Ok,
        Some(c) if c.is_whitespace() || CLOSERS.contains(c) => Boundary::Ok,
        Some('.') => match rest.next() {
            None => Boundary::Ok,
            Some(c) if c.is_whitespace() || CLOSERS.contains(c) => Boundary::Ok,
            Some(_) => Boundary::Compound,
        },
        Some(c) if CONNECTORS.contains(c) => Boundary::Compound,
        Some(_) => Boundary::Other,
    }
}

fn starts(lower: &[u8], at: usize, word: &str) -> bool {
    lower[at..].starts_with(word.as_bytes())
}

fn skip_separators(lower: &[u8], mut at: usize) -> usize {
    while lower
        .get(at)
        .is_some_and(|&b| matches!(b, b' ' | b'\t' | b'-' | b'_'))
    {
        at += 1;
    }
    at
}

fn mass_alias_end(lower: &[u8], at: usize) -> Option<usize> {
    if starts(lower, at, "mulw") {
        return Some(at + 4);
    }
    let (first, second) = if starts(lower, at, "mass") {
        (4, "ulw")
    } else if starts(lower, at, "ulw") {
        (3, "mass")
    } else {
        return None;
    };
    let next = skip_separators(lower, at + first);
    (next > at + first && starts(lower, next, second)).then_some(next + second.len())
}

fn plain_end(lower: &[u8], at: usize) -> Option<usize> {
    if starts(lower, at, "ultrawork") {
        Some(at + 9)
    } else if starts(lower, at, "ulw") {
        Some(at + 3)
    } else {
        None
    }
}

fn boundaries(text: &str, start: usize, end: usize) -> (Boundary, Boundary) {
    (before(text, start), after(text, end))
}

pub fn scan(masked: &str) -> Scan {
    let lower: Vec<u8> = masked.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let mut out = Scan::default();
    let mut index = 0;
    while index < lower.len() {
        let mass_end = mass_alias_end(&lower, index);
        let plain = plain_end(&lower, index);
        // A mass alias only counts as one when its own boundaries hold; otherwise the plain
        // keyword at the same spot still gets its chance.
        let mass_ok = mass_end
            .filter(|&end| matches!(boundaries(masked, index, end), (Boundary::Ok, Boundary::Ok)));
        let (end, is_mass, is_long) = match (mass_ok, plain) {
            (Some(end), _) => (end, true, false),
            (None, Some(end)) => (end, false, end - index == 9),
            (None, None) => {
                index += 1;
                continue;
            }
        };
        match boundaries(masked, index, end) {
            (Boundary::Ok, Boundary::Ok) => {
                out.mentions += 1;
                out.long_form |= is_long;
                out.short_form |= !is_long;
                out.mass |= is_mass;
            }
            (Boundary::Compound, _) | (_, Boundary::Compound) => out.compound_rejected = true,
            _ => {}
        }
        index = end;
    }
    out
}
