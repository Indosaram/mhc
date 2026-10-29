use crate::{settings::OmoSidePanelSections, types::PanelRow};
#[derive(Clone, Debug, Default)]
pub struct PanelRowsInput {
    pub sections: OmoSidePanelSections,
    pub session: Vec<PanelRow>, pub goal: Vec<PanelRow>, pub context: Vec<PanelRow>, pub usage: Vec<PanelRow>,
    pub agents: Vec<PanelRow>, pub tools: Vec<PanelRow>, pub files: Vec<PanelRow>, pub memory: Vec<PanelRow>, pub location: Vec<PanelRow>,
}
pub fn build_panel_rows(input: &PanelRowsInput, width: usize) -> Vec<PanelRow> {
    if width == 0 { return Vec::new(); }
    let blocks = [(input.sections.session,&input.session),(input.sections.goal,&input.goal),(input.sections.context,&input.context),(input.sections.usage,&input.usage),(input.sections.agents,&input.agents),(input.sections.tools,&input.tools),(input.sections.files,&input.files),(input.sections.memory,&input.memory),(true,&input.location)];
    let mut rows = Vec::new();
    for (enabled, block) in blocks {
        if !enabled || block.is_empty() { continue; }
        if !rows.is_empty() { rows.push(PanelRow::default()); }
        rows.extend_from_slice(block);
    }
    rows
}
#[cfg(test)] mod tests {
    use super::*;
    fn input() -> PanelRowsInput { PanelRowsInput { location:vec![PanelRow::new("/srv/app")], ..PanelRowsInput::default() } }
    #[test] fn empty_sections_leave_only_location() { assert_eq!(build_panel_rows(&input(),40),vec![PanelRow::new("/srv/app")]); }
    #[test] fn nonempty_blocks_have_single_separators() { let mut source=input(); source.session=vec![PanelRow::new("SESSION")];source.context=vec![PanelRow::new("CONTEXT  1K/100K")];source.agents=vec![PanelRow::new("AGENTS")];source.tools=vec![PanelRow::new("TOOLS")];let rows=build_panel_rows(&source,46);assert_eq!(rows.iter().filter(|r|r.text.is_empty()).count(),4);assert_eq!(rows.first().map(|r|r.text.as_str()),Some("SESSION"));assert_eq!(rows.last().map(|r|r.text.as_str()),Some("/srv/app")); }
    #[test] fn disabled_sections_are_skipped_without_gaps() { let mut source=input();source.context=vec![PanelRow::new("CONTEXT")];source.sections.context=false;assert_eq!(build_panel_rows(&source,40),source.location);source.sections=OmoSidePanelSections{session:false,goal:false,context:false,usage:false,agents:false,tools:false,files:false,memory:false};assert_eq!(build_panel_rows(&source,40),source.location);assert!(build_panel_rows(&source,0).is_empty()); }
}
