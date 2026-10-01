pub mod stream_utils;
pub mod types;
pub mod coordinator;
pub mod builtin_rules;
pub mod prompts;
pub mod remediation;
pub mod detectors { pub mod collapse_scalars; pub mod collapse_periods; pub mod collapse_lines; pub mod collapse_paragraphs; pub mod repetitive_turns; pub mod collapse_near_duplicates; pub mod collapse; }
