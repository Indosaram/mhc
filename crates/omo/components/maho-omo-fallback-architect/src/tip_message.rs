pub const FALLBACK_ARCHITECT_TIP_TYPE:&str="omo-fallback-architect:tip";
pub fn build_fallback_tip_text(from:&str,to:&str)->String {
    format!("Fable 5 refused, but its refusals should not wear you down: {to} picks up the refused question and reasons through its essence anyway.\nFable-5-grade depth stays reachable through the architect task category, which routes the hard parts back to {from}.\nCurious how this works? Ask about it with the give-me-tips skill.")
}
pub fn render_fallback_tip(text:&str,dim:impl Fn(&str)->String)->Vec<String> {
    text.split('\n').enumerate().map(|(index,line)| {
        let line=senpi_task::renderer_text::normalize_renderer_text(line);
        dim(&if index==0 {format!("Tip: {line}")}else{line})
    }).collect()
}
