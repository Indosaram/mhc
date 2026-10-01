pub mod stream_utils;
pub mod types;
pub mod coordinator;
pub mod builtin_rules;
pub mod prompts;
pub mod remediation;
pub mod repetitive_turns_lane;
pub mod message_update;
pub mod rule_condition;
pub mod scope;
pub mod manager;
pub mod rule_parser;
pub mod discovery;
pub mod watch;
pub mod commands;
pub mod stream_remediation;
pub mod detectors { pub mod collapse_scalars; pub mod collapse_periods; pub mod collapse_lines; pub mod collapse_paragraphs; pub mod repetitive_turns; pub mod collapse_near_duplicates; pub mod collapse; pub mod token_grammar; pub mod leak_context; pub mod control_leak; }
