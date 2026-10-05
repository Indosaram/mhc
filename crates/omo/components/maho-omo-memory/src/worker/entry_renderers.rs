pub use senpi_task::renderer_text::{normalize_renderer_text,optional_renderer_text,join_renderer_tokens};
pub const FIELD_SEPARATOR:&str=" · ";
pub trait EntryRenderTheme:Send+Sync {fn fg(&self,tone:&str,text:&str)->String;fn italic(&self,text:&str)->String;}
pub fn outcome_theme_color(outcome:&str)->&'static str{match outcome{"merged"|"no_changes"=>"success","parent_dirty"|"merge_conflict"|"dirty_uncommitted"|"timed_out"=>"warning","failed"=>"error",_=>"muted"}}
pub fn outcome_glyph(outcome:&str)->&'static str{match outcome_theme_color(outcome){"success"=>"●","error"=>"✗","warning"=>"⚠",_=>"·"}}
pub fn outcome_label(outcome:&str)->String{match outcome{"merged"=>"merged".into(),"no_changes"=>"no changes".into(),"parent_dirty"=>"parent dirty".into(),"merge_conflict"=>"merge conflict".into(),"dirty_uncommitted"=>"dirty worktree".into(),"timed_out"=>"timed out".into(),"failed"=>"failed".into(),_=>normalize_renderer_text(outcome)}}
pub fn outcome_summary(outcome:&str)->&'static str{match outcome{"merged"=>"Reflection merged its findings into memory.","no_changes"=>"Reflection finished with nothing new worth keeping.","parent_dirty"=>"Memory had uncommitted changes, so the merge was skipped.","merge_conflict"=>"The reflection branch conflicted with memory and was left unmerged.","dirty_uncommitted"=>"The reflection worktree ended dirty, so nothing was merged.","timed_out"=>"Reflection hit its deadline; the transcript cursor was not advanced.","failed"=>"Reflection did not finish; the transcript cursor was not advanced.",_=>"Reflection finished with an unrecognised outcome."}}
pub fn fit(text:&str,width:usize)->String{senpi_task::renderer_text::excerpt_renderer_text(text,Some(width))}
pub fn run_label(text:&str)->String{fit(text,28)}
pub fn detail_excerpt(text:&str)->String{fit(text,72)}
pub fn join_fields(fields:&[Option<&str>])->String{fields.iter().flatten().filter(|field|!field.is_empty()).copied().collect::<Vec<_>>().join(FIELD_SEPARATOR)}
pub struct NoticeExtraLine{pub text:String,pub tone:Option<String>}
pub struct NoticeSpec{pub glyph:String,pub title:String,pub tone:String,pub why:String,pub extra:Vec<NoticeExtraLine>,pub detail:Option<String>}
pub struct NoticeComponent{pub spec:NoticeSpec,pub expanded:bool,pub theme:std::sync::Arc<dyn EntryRenderTheme>}
impl maho_ext_api::Component for NoticeComponent {
    fn render(&mut self,width:usize)->Vec<String>{
        if width==0{return vec![String::new()];}
        let title=fit(&format!("{} {}",self.spec.glyph,self.spec.title),width);
        let mut lines=vec![self.theme.fg(&self.spec.tone,&format!("\x1b[1m{title}\x1b[22m")),self.theme.fg("dim",&fit(&self.spec.why,width))];
        for line in &self.spec.extra{if !line.text.is_empty(){lines.push(self.theme.fg(line.tone.as_deref().unwrap_or("dim"),&fit(&line.text,width)));}}
        if self.expanded&&let Some(detail)=self.spec.detail.as_deref().filter(|detail|!detail.is_empty()){lines.push(self.theme.italic(&self.theme.fg("dim",&fit(detail,width))));}
        lines
    }
    fn invalidate(&mut self){}
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::Component;
    struct Theme;
    impl EntryRenderTheme for Theme{fn fg(&self,tone:&str,text:&str)->String{format!("<{tone}>{text}</{tone}>")}fn italic(&self,text:&str)->String{format!("<italic>{text}</italic>")}}
    #[test]
    fn notice_sanitizes_before_style_and_gates_detail(){let mut notice=NoticeComponent{spec:NoticeSpec{glyph:"x".into(),title:"\x1b[31mtitle".into(),tone:"error".into(),why:"why\nline".into(),extra:vec![NoticeExtraLine{text:String::new(),tone:None},NoticeExtraLine{text:"extra".into(),tone:Some("warning".into())}],detail:Some("details".into())},expanded:false,theme:std::sync::Arc::new(Theme)};let lines=notice.render(80);assert_eq!(lines.len(),3);assert!(lines[0].contains("\x1b[1m"));assert!(!lines[0].contains("\x1b[31m"));assert!(lines[2].starts_with("<warning>"));notice.expanded=true;assert_eq!(notice.render(80).len(),4);assert!(notice.render(80)[3].starts_with("<italic>"));assert_eq!(notice.render(0),[""]);}
    #[test]
    fn fields_and_fit_preserve_width_contract(){assert_eq!(join_fields(&[Some("a"),None,Some(""),Some("b")]),format!("a{FIELD_SEPARATOR}b"));for width in 0..12{assert!(senpi_task::renderer_text::renderer_visible_width(&fit("wide 界 text",width))<=width);}}
}
