use super::{
    super::item::{RegisterValue, frontmatter::split_item},
    SpaceError,
    adopt_frontmatter::*,
    hub_trace::{parse_sync_conflict_name, strip_t2_blocks},
    merge::{is_ordered_subsequence, merge_insertions},
    read_back::{first_missing_fact, normalize_input},
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub path: String,
    pub text: String,
}
pub struct ItemState {
    pub current_text: Option<String>,
    pub tombstoned: bool,
    pub last_materialised: Option<String>,
    pub registers: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSource {
    pub path: String,
    pub reason: String,
}
#[derive(Debug)]
pub enum AdoptionAction {
    None,
    Remove,
    Capture {
        working_text: String,
        extra_register_values: Vec<RegisterValue>,
    },
}
#[derive(Debug)]
pub struct AdoptionPlan {
    pub action: AdoptionAction,
    pub rendered: String,
    pub registers: BTreeMap<String, Vec<String>>,
    pub removal_candidates: Vec<String>,
    pub retained_sources: Vec<RetainedSource>,
}
fn unchanged_action(item: &ItemState) -> AdoptionAction {
    if item.tombstoned {
        AdoptionAction::Remove
    } else {
        AdoptionAction::None
    }
}
pub fn plan_adoption(files: &[Snapshot], item: &ItemState) -> Result<AdoptionPlan, SpaceError> {
    plan_adoption_with_merge(files, item, merge_insertions)
}
pub fn plan_adoption_with_merge(
    files: &[Snapshot],
    item: &ItemState,
    merge: impl Fn(&str, &str) -> String,
) -> Result<AdoptionPlan, SpaceError> {
    let (mains, sides): (Vec<_>, Vec<_>) = files
        .iter()
        .partition(|f| parse_sync_conflict_name(&f.path).is_none());
    let base = normalize_input(item.current_text.as_deref().unwrap_or(""));
    if sides.is_empty()
        && mains.len() == 1
        && item
            .last_materialised
            .as_ref()
            .is_some_and(|last| last == &mains[0].text)
    {
        return Ok(AdoptionPlan {
            action: unchanged_action(item),
            rendered: base,
            registers: item.registers.clone(),
            removal_candidates: Vec::new(),
            retained_sources: Vec::new(),
        });
    }
    let mut fields = Vec::new();
    let parsed = split_item(&base);
    seed_fields(
        &mut fields,
        parsed.fm.as_deref().unwrap_or(&[]),
        &item.registers,
    )?;
    let stripped = strip_t2_blocks(&parsed.body);
    adopt_fields(&mut fields, &stripped.entries);
    let mut body = merge("", &stripped.body);
    for (files, main) in [(&mains, true), (&sides, false)] {
        for file in files {
            let text = normalize_input(&file.text);
            let parsed = split_item(&text);
            adopt_fields(&mut fields, parsed.fm.as_deref().unwrap_or(&[]));
            let stripped = strip_t2_blocks(&parsed.body);
            adopt_fields(&mut fields, &stripped.entries);
            let deletion = main
                && item
                    .last_materialised
                    .as_ref()
                    .is_some_and(|last| is_ordered_subsequence(&text, &normalize_input(last)));
            if !deletion {
                body = merge(&body, &stripped.body);
            }
        }
    }
    let working_text = render_fields(&fields, &body, RenderMode::Working)?;
    let extra_register_values = extra_values(&fields);
    let rendered = render_fields(&fields, &body, RenderMode::Final)?;
    let registers = registers_of(&fields);
    let mut removal_candidates = Vec::new();
    let mut retained_sources = Vec::new();
    for side in sides {
        match first_missing_fact(&rendered, &normalize_input(&side.text), &registers) {
            None => removal_candidates.push(side.path.clone()),
            Some(reason) => retained_sources.push(RetainedSource {
                path: side.path.clone(),
                reason,
            }),
        }
    }
    let action = if working_text == base && extra_register_values.is_empty() {
        unchanged_action(item)
    } else {
        AdoptionAction::Capture {
            working_text,
            extra_register_values,
        }
    };
    Ok(AdoptionPlan {
        action,
        rendered,
        registers,
        removal_candidates,
        retained_sources,
    })
}
