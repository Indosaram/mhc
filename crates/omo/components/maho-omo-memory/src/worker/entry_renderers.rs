pub use senpi_task::renderer_text::{normalize_renderer_text,optional_renderer_text,join_renderer_tokens};
pub const FIELD_SEPARATOR:&str=" · ";
pub trait EntryRenderTheme:Send+Sync {
    fn fg(&self,tone:&str,text:&str)->String;
    fn italic(&self,text:&str)->String;
    /// senpi `theme.bg(name, text)`; themes without backgrounds leave the text unstyled.
    fn bg(&self,_name:&str,text:&str)->String{text.to_owned()}
}
/// omo `toneColor`: the notice palette, with `muted` and unknown tones drawn dim.
pub fn notice_tone(tone:&str)->&str{match tone{"accent"|"warning"|"error"|"success"|"dim"=>tone,_=>"dim"}}
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
/// omo `renderNotice`: a `Box(1, 1)` on `customMessageBg` holding the title, the why line, the
/// extra lines and (expanded only) the detail line, each a wrapping `Text`.
pub fn notice_box(spec:&NoticeSpec,expanded:bool,theme:&std::sync::Arc<dyn EntryRenderTheme>)->maho_tui::components::box_::Box{
    use std::{cell::RefCell,rc::Rc};
    use maho_tui::components::text::Text;
    let mut notice=maho_tui::components::box_::Box::with_padding(1,1);
    let background=std::sync::Arc::clone(theme);
    notice.set_bg_fn(Some(Rc::new(move|text:&str|background.bg("customMessageBg",text))));
    let mut add=|tone:&str,text:&str|notice.add_child(Rc::new(RefCell::new(Text::with_padding(theme.fg(notice_tone(tone),&normalize_renderer_text(text)),0,0))));
    add(&spec.tone,&format!("{} {}",spec.glyph,spec.title));
    add("dim",&spec.why);
    for line in spec.extra.iter().filter(|line|!line.text.is_empty()){add(line.tone.as_deref().unwrap_or("dim"),&line.text);}
    if expanded&&let Some(detail)=spec.detail.as_deref().filter(|detail|!detail.is_empty()){add("dim",detail);}
    notice
}
impl maho_ext_api::Component for NoticeComponent {
    fn render(&mut self,width:usize)->Vec<String>{notice_box(&self.spec,self.expanded,&self.theme).render(width)}
    fn invalidate(&mut self){}
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::Component;
    struct Theme;
    impl EntryRenderTheme for Theme{fn fg(&self,tone:&str,text:&str)->String{format!("<{tone}>{text}</{tone}>")}fn italic(&self,text:&str)->String{format!("<italic>{text}</italic>")}}
    #[test]
    fn notice_sanitizes_before_style_and_gates_detail(){let mut notice=NoticeComponent{spec:NoticeSpec{glyph:"x".into(),title:"\x1b[31mtitle".into(),tone:"error".into(),why:"why\nline".into(),extra:vec![NoticeExtraLine{text:String::new(),tone:None},NoticeExtraLine{text:"extra".into(),tone:Some("muted".into())}],detail:Some("details".into())},expanded:false,theme:std::sync::Arc::new(Theme)};let lines=notice.render(80);assert_eq!(lines.len(),5);assert_eq!(lines[0].trim(),"");assert!(lines[1].starts_with(" <error>x title</error>"));assert!(!lines[1].contains("\x1b[31m"));assert!(lines[2].contains("<dim>why line</dim>"));assert!(lines[3].contains("<dim>extra</dim>"));assert!(lines.iter().all(|line|senpi_task::renderer_text::renderer_visible_width(line)>=80));notice.expanded=true;let expanded=notice.render(80);assert_eq!(expanded.len(),6);assert!(expanded[4].contains("<dim>details</dim>"));}
    struct BgTheme;
    impl EntryRenderTheme for BgTheme{fn fg(&self,_:&str,text:&str)->String{text.into()}fn italic(&self,text:&str)->String{text.into()}fn bg(&self,name:&str,text:&str)->String{format!("[{name}]{text}[/]")}}
    #[test]
    fn notice_wraps_long_lines_on_the_custom_message_background(){let mut notice=NoticeComponent{spec:NoticeSpec{glyph:"x".into(),title:"t".into(),tone:"accent".into(),why:"one two three four five six".into(),extra:vec![],detail:None},expanded:false,theme:std::sync::Arc::new(BgTheme)};let lines=notice.render(12);assert!(lines.len()>4,"{lines:?}");assert!(lines.iter().all(|line|line.contains("[customMessageBg]")),"{lines:?}");}
    #[test]
    fn fields_and_fit_preserve_width_contract(){assert_eq!(join_fields(&[Some("a"),None,Some(""),Some("b")]),format!("a{FIELD_SEPARATOR}b"));for width in 0..12{assert!(senpi_task::renderer_text::renderer_visible_width(&fit("wide 界 text",width))<=width);}}
}
