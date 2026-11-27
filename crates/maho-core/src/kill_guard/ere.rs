//! Port of `packages/coding-agent/src/core/kill-guard/ere.ts` (senpi#5349).
//!
//! A POSIX extended regular expression reader, independent of the host's regex engine. It answers
//! the three questions the kill guard asks of a `pkill`/`killall` pattern: does some alternative
//! branch match nothing at all, can the whole pattern match the empty string, and how few literal
//! characters can a match carry.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternReading {
    Unparsable {
        why: String,
    },
    Parsed {
        /// Some alternative branch, at any group level, is empty (`a||b`, `(|x)`, `()`).
        empty_branch: bool,
        /// The whole pattern can match the empty string.
        nullable: bool,
        /// The fewest literal characters any match path carries.
        min_literals: usize,
    },
}

#[derive(Clone, Copy)]
struct Shape {
    nullable: bool,
    min_literals: usize,
}

struct Unreadable(String);

const MAX_PATTERN_LENGTH: usize = 4096;
const MAX_DEPTH: usize = 64;

fn is_any_char_escape(ch: char) -> bool {
    matches!(ch, 'w' | 'W' | 's' | 'S' | 'd' | 'D')
}

fn is_zero_width_escape(ch: char) -> bool {
    matches!(ch, 'b' | 'B' | '<' | '>' | '`' | '\'')
}

struct Reader {
    source: Vec<char>,
    index: usize,
    empty_branch: bool,
}

impl Reader {
    fn new(pattern: &str) -> Self {
        Self {
            source: pattern.chars().collect(),
            index: 0,
            empty_branch: false,
        }
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.index).copied()
    }

    fn read(&mut self) -> Result<Shape, Unreadable> {
        let shape = self.alternation(0)?;
        if self.index < self.source.len() {
            return Err(Unreadable(format!(
                "unbalanced \")\" at offset {}",
                self.index
            )));
        }
        Ok(shape)
    }

    fn alternation(&mut self, depth: usize) -> Result<Shape, Unreadable> {
        if depth > MAX_DEPTH {
            return Err(Unreadable("nested too deeply".into()));
        }
        let mut nullable = false;
        let mut min_literals = usize::MAX;
        loop {
            let branch = self.branch(depth)?;
            nullable |= branch.nullable;
            min_literals = min_literals.min(branch.min_literals);
            if self.peek() == Some('|') {
                self.index += 1;
            } else {
                break;
            }
        }
        Ok(Shape {
            nullable,
            min_literals,
        })
    }

    fn branch(&mut self, depth: usize) -> Result<Shape, Unreadable> {
        let mut nullable = true;
        let mut min_literals = 0usize;
        let mut pieces = 0usize;
        while let Some(ch) = self.peek() {
            if ch == '|' || ch == ')' {
                break;
            }
            let piece = self.piece(depth)?;
            pieces += 1;
            nullable &= piece.nullable;
            min_literals = min_literals.saturating_add(piece.min_literals);
        }
        if pieces == 0 {
            self.empty_branch = true;
        }
        Ok(Shape {
            nullable,
            min_literals,
        })
    }

    fn piece(&mut self, depth: usize) -> Result<Shape, Unreadable> {
        let mut shape = self.atom(depth)?;
        loop {
            match self.peek() {
                Some('*' | '?') => {
                    self.index += 1;
                    shape = Shape {
                        nullable: true,
                        min_literals: 0,
                    };
                }
                Some('+') => {
                    self.index += 1;
                }
                Some('{') => {
                    let minimum = self.interval()?;
                    shape = if minimum == 0 {
                        Shape {
                            nullable: true,
                            min_literals: 0,
                        }
                    } else {
                        Shape {
                            nullable: shape.nullable,
                            min_literals: shape.min_literals.saturating_mul(minimum),
                        }
                    };
                }
                _ => return Ok(shape),
            }
        }
    }

    /// Reads `{m}`, `{m,}` or `{m,n}` and returns `m`; anything else after `{` is a bad interval.
    fn interval(&mut self) -> Result<usize, Unreadable> {
        let start = self.index;
        let mut pos = start + 1;
        let min_start = pos;
        while pos < self.source.len() && self.source[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == min_start {
            return Err(Unreadable(format!("bad interval at offset {start}")));
        }
        let minimum: usize = self.source[min_start..pos]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| Unreadable(format!("bad interval at offset {start}")))?;
        let maximum = match self.source.get(pos).copied() {
            Some('}') => {
                pos += 1;
                Some(minimum)
            }
            Some(',') => {
                pos += 1;
                let max_start = pos;
                while pos < self.source.len() && self.source[pos].is_ascii_digit() {
                    pos += 1;
                }
                if self.source.get(pos).copied() != Some('}') {
                    return Err(Unreadable(format!("bad interval at offset {start}")));
                }
                let max = if pos == max_start {
                    None
                } else {
                    Some(
                        self.source[max_start..pos]
                            .iter()
                            .collect::<String>()
                            .parse::<usize>()
                            .map_err(|_| Unreadable(format!("bad interval at offset {start}")))?,
                    )
                };
                pos += 1;
                max
            }
            _ => return Err(Unreadable(format!("bad interval at offset {start}"))),
        };
        if let Some(maximum) = maximum
            && minimum > maximum
        {
            return Err(Unreadable(format!("bad interval at offset {start}")));
        }
        self.index = pos;
        Ok(minimum)
    }

    fn atom(&mut self, depth: usize) -> Result<Shape, Unreadable> {
        let Some(ch) = self.peek() else {
            return Err(Unreadable("unexpected end of pattern".into()));
        };
        if matches!(ch, '*' | '+' | '?' | '{') {
            return Err(Unreadable(format!(
                "\"{ch}\" at offset {} repeats nothing",
                self.index
            )));
        }
        if ch == '(' {
            self.index += 1;
            let inner = self.alternation(depth + 1)?;
            if self.peek() != Some(')') {
                return Err(Unreadable("unbalanced \"(\"".into()));
            }
            self.index += 1;
            return Ok(inner);
        }
        if ch == '[' {
            self.bracket()?;
            return Ok(Shape {
                nullable: false,
                min_literals: 0,
            });
        }
        if ch == '^' || ch == '$' {
            self.index += 1;
            return Ok(Shape {
                nullable: true,
                min_literals: 0,
            });
        }
        if ch == '.' {
            self.index += 1;
            return Ok(Shape {
                nullable: false,
                min_literals: 0,
            });
        }
        if ch == '\\' {
            let Some(escaped) = self.source.get(self.index + 1).copied() else {
                return Err(Unreadable("trailing backslash".into()));
            };
            self.index += 2;
            if is_any_char_escape(escaped) {
                return Ok(Shape {
                    nullable: false,
                    min_literals: 0,
                });
            }
            if is_zero_width_escape(escaped) {
                return Ok(Shape {
                    nullable: true,
                    min_literals: 0,
                });
            }
            return Ok(Shape {
                nullable: false,
                min_literals: 1,
            });
        }
        self.index += 1;
        Ok(Shape {
            nullable: false,
            min_literals: 1,
        })
    }

    /// Skips a bracket expression starting at `[`; a `]` right after `[` or `[^` is a member, not the end.
    fn bracket(&mut self) -> Result<(), Unreadable> {
        let mut index = self.index + 1;
        if self.source.get(index).copied() == Some('^') {
            index += 1;
        }
        if self.source.get(index).copied() == Some(']') {
            index += 1;
        }
        while index < self.source.len() {
            let ch = self.source[index];
            if ch == ']' {
                self.index = index + 1;
                return Ok(());
            }
            if ch == '['
                && let Some(delimiter) = self
                    .source
                    .get(index + 1)
                    .copied()
                    .filter(|d| matches!(d, ':' | '=' | '.'))
            {
                let mut pos = index + 2;
                let mut found = None;
                while pos + 1 < self.source.len() {
                    if self.source[pos] == delimiter && self.source[pos + 1] == ']' {
                        found = Some(pos + 2);
                        break;
                    }
                    pos += 1;
                }
                let Some(close) = found else {
                    return Err(Unreadable(format!("unterminated \"[{delimiter}\"")));
                };
                index = close;
                continue;
            }
            index += 1;
        }
        Err(Unreadable("unbalanced \"[\"".into()))
    }
}

pub fn read_pattern(pattern: &str) -> PatternReading {
    if pattern.encode_utf16().count() > MAX_PATTERN_LENGTH {
        return PatternReading::Unparsable {
            why: "it is too long to read".into(),
        };
    }
    let mut reader = Reader::new(pattern);
    match reader.read() {
        Ok(shape) => PatternReading::Parsed {
            empty_branch: reader.empty_branch,
            nullable: shape.nullable,
            min_literals: shape.min_literals,
        },
        Err(Unreadable(why)) => PatternReading::Unparsable { why },
    }
}
