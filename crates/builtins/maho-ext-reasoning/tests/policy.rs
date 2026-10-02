use maho_ext_reasoning::*;
use maho_ai::types::{ThinkingLevel,ModelThinkingLevel};
use maho_core::thinking_levels::{ReasoningCapability,ReasoningCapabilityKind};
fn capability()->ReasoningCapability{ReasoningCapability{kind:ReasoningCapabilityKind::Graded,levels:vec![ModelThinkingLevel::Off,ModelThinkingLevel::Low,ModelThinkingLevel::High],non_off_levels:vec![ThinkingLevel::Low,ThinkingLevel::High]}}
#[test]
fn single_argument(){assert_eq!(parse_single_argument("  on  "),Some("on"));assert_eq!(parse_single_argument(""),Some(""));assert_eq!(parse_single_argument("on off"),None);}
#[test]
fn clamp_upward_then_downward(){assert_eq!(clamp_to_non_off(ModelThinkingLevel::Medium,&capability()),Some(ThinkingLevel::High));assert_eq!(clamp_to_non_off(ModelThinkingLevel::Max,&capability()),Some(ThinkingLevel::High));assert_eq!(clamp_to_non_off(ModelThinkingLevel::Off,&capability()),Some(ThinkingLevel::Low));}
#[test]
fn last_on_priority(){assert_eq!(preferred_on_level(Some(ModelThinkingLevel::High),Some(ModelThinkingLevel::Low),Some(ModelThinkingLevel::Low),&capability()),Some(ThinkingLevel::High));}
#[test]
fn off_memory_falls_back(){assert_eq!(preferred_on_level(Some(ModelThinkingLevel::Off),Some(ModelThinkingLevel::Off),Some(ModelThinkingLevel::Low),&capability()),Some(ThinkingLevel::Low));assert_eq!(preferred_on_level(None,None,None,&capability()),Some(ThinkingLevel::High));}
#[test]
fn completion_prefix(){assert_eq!(completions(&["on","off"]," o "),Some(vec!["on","off"]));assert!(completions(&["on","off"],"high").is_none());}
