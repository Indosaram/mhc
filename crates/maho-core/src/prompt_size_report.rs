//! Prompt contribution accounting. Reports contain measurements, never prompt content.
use maho_ai::utils::estimate::estimate_text_tokens;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[derive(Clone, Debug)]
pub struct DynamicSystemPromptPart {
    pub name: String,
    pub source: Option<String>,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSizeEntry {
    pub name: String,
    pub source: String,
    pub bytes: i64,
    pub estimated_tokens: i64,
    pub bounding_settings: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PromptSizeEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_tools: Option<Vec<ToolSize>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSize {
    pub name: String,
    pub bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeTotal {
    pub bytes: i64,
    pub estimated_tokens: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresetPartsMismatch {
    pub names: Vec<String>,
    pub note: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSizeReport {
    pub sections: Vec<PromptSizeEntry>,
    pub total: SizeTotal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_parts_mismatch: Option<PresetPartsMismatch>,
}
fn entry(part: &DynamicSystemPromptPart) -> PromptSizeEntry {
    let mut source = part.source.as_ref().unwrap_or(&part.name).clone();
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        for candidate in [Some(home.clone()), std::fs::canonicalize(home).ok()]
            .into_iter()
            .flatten()
        {
            let path = candidate.to_string_lossy();
            if source == path {
                source = "~".into();
                break;
            }
            if let Some(rest) = source.strip_prefix(&format!("{path}/")) {
                source = format!("~/{rest}");
                break;
            }
        }
    }
    PromptSizeEntry {
        name: part.name.clone(),
        source,
        bytes: part.text.len() as i64,
        estimated_tokens: estimate_text_tokens(&part.text) as i64,
        bounding_settings: (part.name == "project_context").then(|| {
            vec![
                "contextFiles.maxBytesPerFile".into(),
                "contextFiles.maxTotalBytes".into(),
            ]
        }),
        count: None,
        rewritten: None,
        files: None,
        top_tools: None,
    }
}
pub struct PromptSizeAccounting {
    entries: Vec<PromptSizeEntry>,
    skill_count: usize,
    extension_names: HashMap<String, String>,
    preset_parts_mismatch: Vec<String>,
}
impl PromptSizeAccounting {
    pub fn new(
        base: &str,
        skill_count: usize,
        parts: &[DynamicSystemPromptPart],
        extension_names: HashMap<String, String>,
    ) -> Self {
        let rendered: String = parts.iter().map(|p| p.text.as_str()).collect();
        let mut entries = if !parts.is_empty() && base.starts_with(&rendered) {
            parts.iter().map(entry).collect()
        } else {
            vec![entry(&DynamicSystemPromptPart {
                name: "core".into(),
                source: Some("session".into()),
                text: base.into(),
            })]
        };
        if !parts.is_empty() && base.starts_with(&rendered) && base != rendered {
            entries.push(entry(&DynamicSystemPromptPart {
                name: "session_policy".into(),
                source: None,
                text: base[rendered.len()..].into(),
            }));
        }
        Self {
            entries,
            skill_count,
            extension_names,
            preset_parts_mismatch: Vec::new(),
        }
    }
    pub fn observe(
        &mut self,
        path: &str,
        before: &str,
        after: &str,
        parts: Option<&[DynamicSystemPromptPart]>,
    ) {
        let name = self.extension_names.get(path).cloned().unwrap_or_else(|| {
            path.split_once(':')
                .map_or(path, |(_, tail)| tail)
                .trim_end_matches('>')
                .into()
        });
        if let Some(parts) = parts {
            if parts.iter().map(|p| p.text.as_str()).collect::<String>() == after {
                self.entries = parts.iter().map(entry).collect();
                return;
            }
            self.preset_parts_mismatch.push(name.clone());
        }
        if before == after {
            return;
        }
        let added = after
            .strip_prefix(before)
            .or_else(|| after.strip_suffix(before));
        self.entries.push(PromptSizeEntry {
            name: format!("extension:{name}"),
            source: name,
            bytes: after.len() as i64 - before.len() as i64,
            estimated_tokens: added.map_or_else(
                || estimate_text_tokens(after) as i64 - estimate_text_tokens(before) as i64,
                |text| estimate_text_tokens(text) as i64,
            ),
            bounding_settings: None,
            count: None,
            rewritten: added.is_none().then_some(true),
            files: None,
            top_tools: None,
        });
    }
    pub fn report(&self, tools: &[maho_ai::types::Tool]) -> PromptSizeReport {
        let mut sections = Vec::new();
        let mut project = Vec::new();
        for part in &self.entries {
            if part.name == "project_context" {
                project.push(part.clone());
                continue;
            }
            let mut part = part.clone();
            if part.name == "skills" {
                part.count = Some(self.skill_count);
                part.bounding_settings = Some(vec![
                    "MAX_SKILL_CATALOG_BYTES (49152)".into(),
                    "COMPACT_SKILL_CATALOG_COUNT (100)".into(),
                    "COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES (65536)".into(),
                ]);
            }
            sections.push(part);
        }
        if let Some(first) = project.first() {
            let mut aggregate = first.clone();
            aggregate.source = if first.source.starts_with("prompt-preset:") {
                first.source.clone()
            } else {
                "context files".into()
            };
            aggregate.bytes = project.iter().map(|p| p.bytes).sum();
            aggregate.estimated_tokens = project.iter().map(|p| p.estimated_tokens).sum();
            project.retain(|p| {
                p.source != "dynamic-prompt" && !p.source.starts_with("prompt-preset:")
            });
            project.sort_by_key(|p| std::cmp::Reverse(p.bytes));
            aggregate.files = Some(project);
            sections.push(aggregate);
        }
        let serialized = serde_json::to_string(tools).expect("tools serialize");
        let mut tool_entry = entry(&DynamicSystemPromptPart {
            name: "tools".into(),
            source: Some("provider declarations".into()),
            text: serialized,
        });
        tool_entry.count = Some(tools.len());
        let mut top: Vec<_> = tools
            .iter()
            .map(|t| ToolSize {
                name: t.name.clone(),
                bytes: serde_json::to_vec(t).expect("tool serializes").len(),
            })
            .collect();
        top.sort_by_key(|t| std::cmp::Reverse(t.bytes));
        top.truncate(10);
        tool_entry.top_tools = Some(top);
        sections.push(tool_entry);
        sections.sort_by_key(|p| std::cmp::Reverse(p.bytes));
        let total = SizeTotal {
            bytes: sections.iter().map(|p| p.bytes).sum(),
            estimated_tokens: sections.iter().map(|p| p.estimated_tokens).sum(),
        };
        PromptSizeReport {
            sections,
            total,
            preset_parts_mismatch: (!self.preset_parts_mismatch.is_empty()).then(|| {
                PresetPartsMismatch {
                    names: self.preset_parts_mismatch.clone(),
                    note: "replacement parts did not join to its prompt; its row is the net delta"
                        .into(),
                }
            }),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measurements_do_not_leak_text() {
        let mut a = PromptSizeAccounting::new("secret", 0, &[], HashMap::new());
        a.observe("x", "secret", "secret plus", None);
        let report = a.report(&[]);
        assert_eq!(report.total.bytes, 13);
        assert!(
            !serde_json::to_string(&report)
                .expect("report")
                .contains("secret")
        );
    }
}
