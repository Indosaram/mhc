use serde_json::{Map, Value};

use crate::types::DiscoveredLegacyConfigSource;

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedLegacyConfigSource {
    pub path: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigMigrationTransformResult {
    pub diagnostics: Vec<String>,
    pub document: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeTransformScope {
    User,
    Project { project_root: String },
}

#[derive(Debug, Clone, Copy)]
pub struct TransformOpenCodeSourcesInput<'a> {
    pub discovered: &'a [DiscoveredLegacyConfigSource],
    pub scope: &'a OpenCodeTransformScope,
    pub sources: &'a [LoadedLegacyConfigSource],
}

#[derive(Debug, Clone, Copy)]
pub struct TransformConfigJsoncSourcesInput<'a> {
    pub discovered: &'a [DiscoveredLegacyConfigSource],
    pub sources: &'a [LoadedLegacyConfigSource],
}
