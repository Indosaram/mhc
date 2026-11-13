//! Replaces inactive regions of a prompt with spaces, keeping byte length and newlines.

use crate::mask_phrases::{NEGATIONS, header_spans, prohibitive_spans};

pub(crate) type Span = (usize, usize);

/// A prompt with its inactive regions blanked out.
pub struct Masked {
    /// All passes applied.
    pub text: String,
    /// Everything except the prohibitive-phrasing pass; tells a negation apart from a plain miss.
    pub before_prohibitive: String,
    /// The prompt carries a live contract wrapper tag (outside code, quotes and quotes-like regions).
    pub embedded_directive: bool,
}

/// A pass runs only when the text carries a substring its detector requires. Masking replaces
/// characters with spaces and never introduces one, so a marker missing from the original text is
/// missing from every later intermediate too, that pass would find no span, and skipping it is
/// observably a no-op. Every detector below allocates per line or per word, so a typical draft that
/// carries no code, quote, URL or negation skips all of them.
pub fn mask(input: &str, directive_tag: &str) -> Masked {
    let mut text = if input.contains("```") || input.contains("~~~") {
        blank(input, &fence_spans(input))
    } else {
        input.to_owned()
    };
    if text.contains('>') {
        text = blank(&text, &blockquote_spans(&text));
    }
    if text.contains('`') {
        text = blank(&text, &code_span_spans(&text));
    }
    if text.contains('"') || text.contains('\u{201C}') {
        text = blank(&text, &quote_spans(&text));
    }
    if text.contains("://") {
        text = blank(&text, &url_spans(&text));
    }
    let embedded_directive = text.contains(directive_tag);
    if text.contains('<') {
        text = blank(&text, &tag_pair_spans(&text));
    }
    if text.contains(':') || text.contains('[') {
        text = blank(&text, &header_spans(&text));
    }
    let before_prohibitive = text.clone();
    if negates(&text) {
        text = blank(&text, &prohibitive_spans(&text));
    }
    Masked {
        text,
        before_prohibitive,
        embedded_directive,
    }
}

/// Whether the text carries any word [`prohibitive_spans`] could start from. Case-insensitive and
/// linear: it only decides whether the word-by-word scan is worth running. Reads the same table the
/// detector reads, and folds exactly the way its `normalise` does, so the two cannot drift apart.
fn negates(text: &str) -> bool {
    let lower = text.to_lowercase().replace('\u{2019}', "'");
    NEGATIONS.iter().any(|token| lower.contains(token))
}

fn blank(text: &str, spans: &[Span]) -> String {
    if spans.is_empty() {
        return text.to_owned();
    }
    let mut flags = vec![false; text.len()];
    for &(start, end) in spans {
        flags[start..end.min(text.len())].fill(true);
    }
    let mut blanked = String::with_capacity(text.len());
    for (index, ch) in text.char_indices() {
        if flags[index] && ch != '\n' {
            blanked.extend(std::iter::repeat_n(' ', ch.len_utf8()));
        } else {
            blanked.push(ch);
        }
    }
    blanked
}

pub(crate) fn lines(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            out.push((start, index));
            start = index + 1;
        }
    }
    out.push((start, text.len()));
    out
}

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let marker = *rest.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let run = rest.bytes().take_while(|&b| b == marker).count();
    let info_has_backtick = marker == b'`' && rest[run..].contains('`');
    (run >= 3 && !info_has_backtick).then_some((marker, run))
}

fn fence_closes(line: &str, marker: u8, run: usize) -> bool {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return false;
    }
    let found = rest.bytes().take_while(|&b| b == marker).count();
    found >= run && rest[found..].trim().is_empty()
}

fn fence_spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut open: Option<(usize, u8, usize)> = None;
    for (start, end) in lines(text) {
        let line = &text[start..end];
        match open {
            None => {
                if let Some((marker, run)) = fence_open(line) {
                    open = Some((start, marker, run));
                }
            }
            Some((from, marker, run)) => {
                if fence_closes(line, marker, run) {
                    spans.push((from, end));
                    open = None;
                }
            }
        }
    }
    if let Some((from, _, _)) = open {
        spans.push((from, text.len()));
    }
    spans
}

fn blockquote_spans(text: &str) -> Vec<Span> {
    lines(text)
        .into_iter()
        .filter(|&(start, end)| {
            let line = &text[start..end];
            let rest = line.trim_start_matches(' ');
            line.len() - rest.len() <= 3 && rest.starts_with('>')
        })
        .collect()
}

fn backtick_run(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().take_while(|&&b| b == b'`').count()
}

fn code_span_spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run = backtick_run(bytes, i);
        let mut j = i + run;
        let mut closing = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let other = backtick_run(bytes, j);
                if other == run {
                    closing = Some(j + other);
                    break;
                }
                j += other;
            } else {
                j += 1;
            }
        }
        match closing {
            Some(end) => {
                spans.push((i, end));
                i = end;
            }
            None => i += run,
        }
    }
    spans
}

fn quote_spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut skip_until = 0;
    for (index, ch) in text.char_indices() {
        if index < skip_until {
            continue;
        }
        let closer = match ch {
            '"' => '"',
            '\u{201C}' => '\u{201D}',
            _ => continue,
        };
        let after = index + ch.len_utf8();
        if let Some(offset) = text[after..].find(closer) {
            let end = after + offset + closer.len_utf8();
            spans.push((index, end));
            skip_until = end;
        }
    }
    spans
}

fn url_spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find("://") {
        let at = from + offset;
        let mut start = at;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric() || b"+-.".contains(&bytes[start - 1]))
        {
            start -= 1;
        }
        let mut end = at + 3;
        while end < bytes.len()
            && !bytes[end].is_ascii_whitespace()
            && !b"<>\")".contains(&bytes[end])
        {
            end += 1;
        }
        if start < at {
            spans.push((start, end));
        }
        from = end;
    }
    spans
}

fn tag_pair_spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' || !bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            continue;
        }
        let name_end = (i
            + 1
            + bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || b"_:.-".contains(*b))
                .count())
        .min(bytes.len());
        let name = &text[i + 1..name_end];
        let closed = text[name_end..]
            .find(['>', '<'])
            .filter(|&k| bytes[name_end + k] == b'>')
            .map(|k| name_end + k + 1)
            .filter(|&open_end| !text[..open_end].ends_with("/>"))
            .and_then(|open_end| {
                let closing = format!("</{name}>");
                text[open_end..]
                    .find(&closing)
                    .map(|k| open_end + k + closing.len())
            });
        match closed {
            Some(end) => {
                spans.push((i, end));
                i = end;
            }
            None => i = name_end,
        }
    }
    spans
}
