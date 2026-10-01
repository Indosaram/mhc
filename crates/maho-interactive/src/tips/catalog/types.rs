//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/types.ts`.

pub type TipRender = fn(&dyn Fn(&str) -> String) -> String;

#[derive(Clone, Copy)]
pub struct TipDefinition {
    pub id: &'static str,
    pub bindings: &'static [&'static str],
    pub requires_command: Option<&'static str>,
    pub render: TipRender,
}
