//! Port of `permission-system/auto-shell-grammar.ts` (senpi 903b8336).

use crate::auto_shell_segments::ShellWord;
use std::{collections::HashMap, sync::Arc};

/// What a word means to the program: `ReadFile` is read from (must be a regular project file),
/// `List` only has its name shown, `Text` is never used as a path, and `RefOrPath` is a git revision
/// unless something exists at that name, in which case it is checked like `List`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordRole {
    ReadFile,
    List,
    Text,
    RefOrPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedWord {
    pub role: WordRole,
    pub word: ShellWord,
}

/// A flag either takes no value (`true` upstream) or one value with the given role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagSpec {
    NoValue,
    Value(WordRole),
}

/// The role of operand `index` out of `count` operands, given its text.
pub type OperandRole = Arc<dyn Fn(usize, usize, &str) -> WordRole + Send + Sync>;

#[derive(Clone)]
pub struct ProgramSpec {
    /// Every flag the program may take.
    pub flags: HashMap<String, FlagSpec>,
    pub operand: OperandRole,
    pub min_operands: Option<usize>,
    pub max_operands: Option<usize>,
}

/// Classifies every word of one simple command against `spec`, or returns `None` when any word is
/// not understood: an unknown flag (short or long), a flag in a form the spec does not list, a
/// value-taking flag with no value, or an operand count out of range. Short flags may be clustered
/// (`-la`), and a value-taking short flag takes the rest of its word as the value (`-o/x`, `-ro/x`)
/// or else the next word; a long flag takes `--name=value` or `--name value`.
pub fn classify_words(words: &[ShellWord], spec: &ProgramSpec) -> Option<Vec<ClassifiedWord>> {
    let mut classified = Vec::new();
    let mut operands: Vec<&ShellWord> = Vec::new();
    let mut options_ended = false;
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        let text = word.text.as_str();
        if options_ended || text == "-" || !text.starts_with('-') {
            operands.push(word);
            index += 1;
            continue;
        }
        if text == "--" {
            options_ended = true;
            index += 1;
            continue;
        }
        if text.starts_with("--") {
            let separator = text.find('=');
            let name = separator.map_or(text, |at| &text[..at]);
            match *spec.flags.get(name)? {
                FlagSpec::NoValue => {
                    if separator.is_some() {
                        return None;
                    }
                }
                FlagSpec::Value(role) => {
                    if let Some(at) = separator {
                        classified.push(ClassifiedWord {
                            role,
                            word: ShellWord {
                                text: text[at + 1..].to_owned(),
                            },
                        });
                    } else {
                        let value = words.get(index + 1)?;
                        classified.push(ClassifiedWord {
                            role,
                            word: value.clone(),
                        });
                        index += 1;
                    }
                }
            }
            index += 1;
            continue;
        }
        let chars: Vec<char> = text.chars().collect();
        for position in 1..chars.len() {
            match *spec.flags.get(&format!("-{}", chars[position]))? {
                FlagSpec::NoValue => continue,
                FlagSpec::Value(role) => {
                    let attached: String = chars[position + 1..].iter().collect();
                    if !attached.is_empty() {
                        classified.push(ClassifiedWord {
                            role,
                            word: ShellWord { text: attached },
                        });
                    } else {
                        let value = words.get(index + 1)?;
                        classified.push(ClassifiedWord {
                            role,
                            word: value.clone(),
                        });
                        index += 1;
                    }
                    break;
                }
            }
        }
        index += 1;
    }
    if operands.len() < spec.min_operands.unwrap_or(0) {
        return None;
    }
    if spec.max_operands.is_some_and(|max| operands.len() > max) {
        return None;
    }
    let count = operands.len();
    for (index, word) in operands.into_iter().enumerate() {
        classified.push(ClassifiedWord {
            role: (spec.operand)(index, count, &word.text),
            word: word.clone(),
        });
    }
    Some(classified)
}
