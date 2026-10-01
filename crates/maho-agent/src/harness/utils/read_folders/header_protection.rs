use super::{lexical_context::Open, types::ReadLineRange};
#[derive(Debug)]
struct Header {
    kind: String,
    depth: usize,
    start_line: usize,
}
#[derive(Debug, Default)]
pub struct HeaderProtection {
    pub intervals: Vec<ReadLineRange>,
    pending: Option<Header>,
    target: Option<usize>,
    parameters: Option<(usize, usize)>,
}
impl HeaderProtection {
    pub fn active(&self) -> bool {
        self.pending.is_some()
    }
    pub fn unfinished(&self) -> bool {
        self.pending.is_some()
    }
    pub fn word(
        &mut self,
        word: &str,
        previous: &str,
        depth: usize,
        line: usize,
        typescript: bool,
    ) -> bool {
        self.target = None;
        if typescript && ["keyof", "typeof", "infer", "readonly", "satisfies", "as"].contains(&word)
        {
            return false;
        }
        if (word == "class" || word == "function") && previous != "." {
            if self.pending.is_some() {
                return false;
            }
            self.pending = Some(Header {
                kind: word.into(),
                depth,
                start_line: line,
            });
        }
        true
    }
    pub fn open(&mut self, c: char, depth: usize, previous: &str, line: usize) -> bool {
        let Some(header) = self.pending.as_ref() else {
            return false;
        };
        if c != '{' || header.depth != depth || (header.kind == "function" && previous != ")") {
            return false;
        }
        let class = header.kind == "class";
        self.protect(header.start_line, line);
        self.pending = None;
        self.parameters = None;
        class
    }
    pub fn close(&mut self, open: &Open, depth: usize, line: usize) {
        self.target = open.target.then_some(open.line);
        let start = open.header_line.unwrap_or(open.line);
        if open.protected && (open.signature || open.char != '(') {
            self.protect(start, line);
        }
        if open.char == '(' && !open.call && !open.control {
            self.parameters = Some((depth, start));
        }
    }
    pub fn punctuation(&mut self, token: &str, depth: usize, line: usize) -> bool {
        if let Some(target) = self.target.take()
            && token == "="
        {
            self.protect(target, line);
        }
        if let Some((parameter_depth, start)) = self.parameters {
            if token == ":" && parameter_depth == depth {
                return false;
            }
            if (token == "{" || token == "=>") && parameter_depth == depth {
                self.protect(start, line);
                self.parameters = None;
            } else if [";", "=", ","].contains(&token) {
                self.parameters = None;
            }
        }
        true
    }
    pub fn protect(&mut self, start_line: usize, end_line: usize) {
        self.intervals.push(ReadLineRange {
            start_line,
            end_line,
        });
    }
    pub fn filter(&self, ranges: Vec<ReadLineRange>) -> Vec<ReadLineRange> {
        ranges
            .into_iter()
            .filter(|r| {
                !self
                    .intervals
                    .iter()
                    .any(|h| r.start_line <= h.end_line && r.end_line >= h.start_line)
            })
            .collect()
    }
}
