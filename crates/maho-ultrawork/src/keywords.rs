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
/// Skill words a mass alias may run into (`mass ulw-loop`), as upstream allows.
const CHAIN_WORDS: [&str; 5] = ["loop", "plan", "research", "execute", "mass"];

enum Boundary {
    Ok,
    Compound,
    Other,
}

// Upstream arms on JS `\b` without the `u` flag: only ASCII letters, digits and `_` are word
// characters, so `ulw\u{d574}\u{c918}` and `\u{e9}ulw` stand on their own.
fn before(text: &str, at: usize) -> Boundary {
    match text[..at].chars().next_back() {
        Some(c) if CONNECTORS.contains(c) => Boundary::Compound,
        Some(c) if c.is_ascii_alphanumeric() => Boundary::Other,
        _ => Boundary::Ok,
    }
}

fn after(text: &str, at: usize) -> Boundary {
    let mut rest = text[at..].chars();
    match rest.next() {
        Some('.') => match rest.next() {
            Some(c) if c.is_ascii_alphanumeric() || CONNECTORS.contains(c) => Boundary::Compound,
            _ => Boundary::Ok,
        },
        Some(c) if CONNECTORS.contains(c) => Boundary::Compound,
        Some(c) if c.is_ascii_alphanumeric() => Boundary::Other,
        _ => Boundary::Ok,
    }
}

fn starts(lower: &[u8], at: usize, word: &str) -> bool {
    lower[at..].starts_with(word.as_bytes())
}

fn skip_separators(lower: &[u8], mut at: usize) -> usize {
    while lower
        .get(at)
        .is_some_and(|&b| b.is_ascii_whitespace() || matches!(b, b'-' | b'_'))
    {
        at += 1;
    }
    at
}

fn mass_alias_end(lower: &[u8], at: usize) -> Option<usize> {
    if starts(lower, at, "mulw") || starts(lower, at, "meth") {
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
    starts(lower, next, second).then_some(next + second.len())
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

/// The chained word ends its own segment and opens no path (`mass ulw-loop.md` does not count).
fn segment_ends(lower: &[u8], at: usize) -> bool {
    match lower.get(at) {
        Some(&b) if b.is_ascii_alphanumeric() || b == b'/' || b == b'\\' => false,
        Some(b'-' | b'_' | b'.') => !lower.get(at + 1).is_some_and(u8::is_ascii_alphanumeric),
        _ => true,
    }
}

fn chained_skill_word(lower: &[u8], at: usize) -> bool {
    matches!(lower.get(at), Some(b'-' | b'_'))
        && CHAIN_WORDS
            .iter()
            .any(|word| starts(lower, at + 1, word) && segment_ends(lower, at + 1 + word.len()))
}

fn mass_boundaries(text: &str, lower: &[u8], start: usize, end: usize) -> (Boundary, Boundary) {
    match boundaries(text, start, end) {
        (left, Boundary::Compound) if chained_skill_word(lower, end) => (left, Boundary::Ok),
        found => found,
    }
}

/// One keyword the scanner stopped on, with the boundary verdict.
struct Hit {
    start: usize,
    end: usize,
    mass: bool,
    long: bool,
    boundaries: (Boundary, Boundary),
}

fn hits(masked: &str) -> Vec<Hit> {
    let lower: Vec<u8> = masked.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < lower.len() {
        let mass_end = mass_alias_end(&lower, index);
        let plain = plain_end(&lower, index);
        // A mass alias only counts as one when its own boundaries hold; otherwise the plain
        // keyword at the same spot still gets its chance.
        let mass_ok = mass_end.filter(|&end| {
            matches!(
                mass_boundaries(masked, &lower, index, end),
                (Boundary::Ok, Boundary::Ok)
            )
        });
        let (end, is_mass, is_long) = match (mass_ok, plain) {
            (Some(end), _) => (end, true, false),
            (None, Some(end)) => (end, false, end - index == 9),
            (None, None) => {
                index += 1;
                continue;
            }
        };
        out.push(Hit {
            start: index,
            end,
            mass: is_mass,
            long: is_long,
            boundaries: if is_mass {
                mass_boundaries(masked, &lower, index, end)
            } else {
                boundaries(masked, index, end)
            },
        });
        index = end;
    }
    out
}

pub fn scan(masked: &str) -> Scan {
    let mut out = Scan::default();
    for hit in hits(masked) {
        match hit.boundaries {
            (Boundary::Ok, Boundary::Ok) => {
                out.mentions += 1;
                out.long_form |= hit.long;
                out.short_form |= !hit.long;
                out.mass |= hit.mass;
            }
            (Boundary::Compound, _) | (_, Boundary::Compound) => out.compound_rejected = true,
            _ => {}
        }
    }
    out
}

/// Byte spans `(start, end, mass)` of the mentions [`scan`] counts, in text order.
pub fn mention_spans(masked: &str) -> Vec<(usize, usize, bool)> {
    hits(masked)
        .into_iter()
        .filter(|hit| matches!(hit.boundaries, (Boundary::Ok, Boundary::Ok)))
        .map(|hit| (hit.start, hit.end, hit.mass))
        .collect()
}
