use maho_ext_todotools::{normalize::normalize_todo_params,todo_types::*,markdown::*};
use serde_json::{Value,json};
fn rejected(raw:Value) {let saved=raw.clone();let result=normalize_todo_params(&raw,&[]);assert!(result.entry.is_none());assert!(result.error.is_some());assert!(result.corrections.is_empty());assert_eq!(raw,saved);}
#[test] fn blank_start_task(){rejected(json!({"op":"start","task":" \t "}));}
#[test] fn blank_start_phase(){rejected(json!({"op":"start","phase":" \t "}));}
#[test] fn blank_done_task(){rejected(json!({"op":"done","task":" \t "}));}
#[test] fn blank_done_phase(){rejected(json!({"op":"done","phase":" \t "}));}
#[test] fn blank_drop_task(){rejected(json!({"op":"drop","task":" \t "}));}
#[test] fn blank_drop_phase(){rejected(json!({"op":"drop","phase":" \t "}));}
#[test] fn blank_rm_task(){rejected(json!({"op":"rm","task":" \t "}));}
#[test] fn blank_rm_phase(){rejected(json!({"op":"rm","phase":" \t "}));}
#[test] fn incoherent_init_task(){rejected(json!({"op":"init","task":"x"}));}
#[test] fn incoherent_start_phase(){rejected(json!({"op":"start","phase":"Tasks"}));}
#[test] fn incoherent_start_list(){rejected(json!({"op":"start","list":[{"phase":"A","items":["x"]}]}));}
#[test] fn incoherent_drop_items(){rejected(json!({"op":"drop","items":["x"]}));}
#[test] fn incoherent_rm_list(){rejected(json!({"op":"rm","list":[{"phase":"A","items":["x"]}]}));}
#[test] fn incoherent_append_list(){rejected(json!({"op":"append","list":[{"phase":"A","items":["x"]}]}));}
#[test] fn incoherent_append_task(){rejected(json!({"op":"append","task":"x","items":["y"]}));}
#[test] fn both_blank_start(){rejected(json!({"op":"start","task":"","phase":""}));}
#[test] fn both_blank_done(){rejected(json!({"op":"done","task":"","phase":""}));}
#[test] fn both_blank_drop(){rejected(json!({"op":"drop","task":"","phase":""}));}

#[test]
fn source_two_phase_markdown_roundtrip() {
    let phases=vec![TodoPhase{name:"Foundation".into(),tasks:vec![TodoItem{content:"Done work".into(),status:TodoStatus::Completed},TodoItem{content:"Active work".into(),status:TodoStatus::InProgress},TodoItem{content:"Dropped work".into(),status:TodoStatus::Abandoned}]},TodoPhase{name:"Verification".into(),tasks:vec![TodoItem{content:"Open work".into(),status:TodoStatus::Pending}]}];
    let markdown=phases_to_markdown(&phases);
    assert_eq!(markdown,"# Foundation\n- [x] Done work\n- [/] Active work\n- [-] Dropped work\n\n# Verification\n- [ ] Open work\n");
    let parsed=markdown_to_phases(&markdown);
    assert!(parsed.errors.is_empty());
    assert_eq!(parsed.phases,phases);
}
#[test]
fn source_headerless_active_tasks_normalize() {
    let parsed=markdown_to_phases("- [/] First\n- [/] Second\n");
    assert!(parsed.errors.is_empty());
    assert_eq!(parsed.phases,vec![TodoPhase{name:"Tasks".into(),tasks:vec![TodoItem{content:"First".into(),status:TodoStatus::InProgress},TodoItem{content:"Second".into(),status:TodoStatus::Pending}]}]);
}
#[test] fn source_default_markdown_path(){assert_eq!(resolve_todo_markdown_path("",std::path::Path::new("/work")),std::path::Path::new("/work/TODO.md"));}
#[test] fn source_relative_markdown_path(){assert_eq!(resolve_todo_markdown_path("notes/plan.md",std::path::Path::new("/work")),std::path::Path::new("/work/notes/plan.md"));}
#[test] fn source_quoted_absolute_markdown_path(){assert_eq!(resolve_todo_markdown_path("\"/abs/plan.md\"",std::path::Path::new("/work")),std::path::Path::new("/abs/plan.md"));}
