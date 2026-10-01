use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct QuestionOption { pub label: String, pub description: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Question { pub id: String, pub header: String, pub question: String, pub options: Vec<QuestionOption>, #[serde(default)] pub multi_select: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct QuestionRequest { pub request_id: String, pub questions: Vec<Question>, pub wait_for_answer: bool, pub timeout_ms: u64 }
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer { pub selected: Vec<String>, #[serde(skip_serializing_if="Option::is_none")] pub text: Option<String> }
pub type QuestionAnswers = BTreeMap<String, QuestionAnswer>;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuestionDraft { #[serde(default)] pub answers: QuestionAnswers, #[serde(skip_serializing_if="Option::is_none")] pub comment: Option<String> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionStatus { #[serde(rename="answered")] Answered, #[serde(rename="comment-submitted")] CommentSubmitted, #[serde(rename="cancelled")] Cancelled, #[serde(rename="timed_out")] TimedOut }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct QuestionResponse { pub status: QuestionStatus, pub answers: QuestionAnswers, #[serde(skip_serializing_if="Option::is_none")] pub comment: Option<String>, pub unanswered: Vec<String>, #[serde(skip_serializing_if="Option::is_none")] pub auto_resolved_after_ms: Option<u64> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionFocus { Options, OwnAnswer, Submit }
pub const COMMENT_LABEL: &str = "Comment (optional; unanswered questions are reported)";
pub const OWN_ANSWER_LABEL: &str = "Type your own answer...";
pub const NOT_ANSWERED_NOTICE: &str = "You have not answered all questions";
pub const DISMISS_NOTICE: &str = "Press Esc again to dismiss (answers will be discarded)";
pub const PARTIAL_SUBMIT_NOTICE: &str = "Press Enter again to submit with unanswered questions";
pub fn format_countdown_label(remaining_ms: f64) -> String {
    let seconds = (remaining_ms / 1000.0).ceil().max(0.0) as u64;
    if seconds >= 300 { format!("{}m", seconds.div_ceil(60)) } else { format!("{:02}:{:02}", seconds / 60, seconds % 60) }
}
pub struct AskUserQuestionState {
    pub request: QuestionRequest, pub active_index: usize, pub highlight_index: usize, pub focus: QuestionFocus, pub submit_row_index: usize, pub notice: Option<String>, pub comment: Option<String>, dismiss_pending: bool, selected: BTreeMap<String, Vec<String>>, texts: BTreeMap<String, String>,
}
impl AskUserQuestionState {
    pub fn new(request: QuestionRequest) -> Self { let submit_row_index=request.questions.len(); Self { request, active_index:0, highlight_index:0, focus:QuestionFocus::Options, submit_row_index, notice:None, comment:None, dismiss_pending:false, selected:BTreeMap::new(), texts:BTreeMap::new() } }
    pub fn active_question(&self) -> &Question { &self.request.questions[self.active_index] }
    pub fn own_answer_row_index(&self) -> usize { self.active_question().options.len() }
    pub fn row_count(&self) -> usize { self.own_answer_row_index()+1 }
    pub fn active_tab_index(&self) -> usize { if self.focus==QuestionFocus::Submit { self.comment_row_index() } else { self.active_index } }
    pub fn comment_row_index(&self) -> usize { self.request.questions.len() }
    pub fn is_comment_focused(&self) -> bool { self.focus==QuestionFocus::Submit && self.submit_row_index==self.comment_row_index() }
    pub fn switch_question(&mut self, delta:isize) { self.switch_tab(delta); }
    pub fn switch_tab(&mut self, delta:isize) { let count=self.request.questions.len()+1; let next=(self.active_tab_index() as isize+delta).rem_euclid(count as isize) as usize; if next==self.comment_row_index() { self.enter_submit(); } else { self.jump_to_question(next); } }
    pub fn advance(&mut self) { if self.active_index+1<self.request.questions.len() { self.jump_to_question(self.active_index+1); } else { self.enter_submit(); } }
    pub fn jump_to_question(&mut self,index:usize) { self.focus=QuestionFocus::Options; self.active_index=index; self.highlight_index=0; self.clear_transient(); }
    pub fn enter_submit(&mut self) { self.focus=QuestionFocus::Submit; self.submit_row_index=self.comment_row_index(); self.highlight_index=0; self.clear_transient(); }
    pub fn return_to_options(&mut self) { self.leave_own_answer(0); }
    pub fn leave_own_answer(&mut self,row:isize) { self.focus=QuestionFocus::Options; self.highlight_index=row.max(0).min(self.own_answer_row_index() as isize) as usize; self.clear_transient(); }
    pub fn move_submit_row(&mut self,delta:isize) { self.submit_row_index=(self.submit_row_index as isize+delta).clamp(0,self.comment_row_index() as isize) as usize; self.clear_transient(); }
    pub fn focus_comment(&mut self) { self.submit_row_index=self.comment_row_index(); }
    fn clear_transient(&mut self) { self.notice=None; self.dismiss_pending=false; }
    pub fn request_dismiss(&mut self) -> bool { if !self.has_draft() || self.dismiss_pending { return true; } self.dismiss_pending=true; self.notice=Some(DISMISS_NOTICE.into()); false }
    pub fn accept_partial_submit(&mut self) { self.clear_transient(); }
    fn has_draft(&self) -> bool { self.comment.as_ref().is_some_and(|s| !s.trim().is_empty()) || self.answered_count()>0 }
    pub fn selected_for(&self,id:&str) -> &[String] { self.selected.get(id).map_or(&[],Vec::as_slice) }
    pub fn text_for(&self,id:&str) -> Option<&str> { self.texts.get(id).map(String::as_str) }
    pub fn is_selected(&self,id:&str,label:&str) -> bool { self.selected_for(id).iter().any(|s| s==label) }
    pub fn is_answered(&self,id:&str) -> bool { !self.selected_for(id).is_empty() || self.text_for(id).is_some_and(|s| !s.trim().is_empty()) }
    pub fn answered_count(&self) -> usize { self.request.questions.iter().filter(|q| self.is_answered(&q.id)).count() }
    pub fn activate_option(&mut self,id:&str,label:&str) {
        let Some(q)=self.request.questions.iter().find(|q| q.id==id) else { return; };
        if !q.options.iter().any(|o| o.label==label) { return; }
        if q.multi_select { let values=self.selected.entry(id.into()).or_default(); if let Some(i)=values.iter().position(|s| s==label) { values.remove(i); } else { values.push(label.into()); } } else { self.selected.insert(id.into(),vec![label.into()]); } self.notice=None;
    }
    pub fn set_own_answer(&mut self,id:&str,text:&str) { let text=text.trim(); if text.is_empty() { return; } self.selected.remove(id); self.texts.insert(id.into(),text.into()); self.notice=None; }
    pub fn clear_answer(&mut self,id:&str) { self.selected.remove(id); self.texts.remove(id); self.notice=None; }
    pub fn restore_draft(&mut self,draft:QuestionDraft) { for q in &self.request.questions { if let Some(a)=draft.answers.get(&q.id) { let selected:Vec<_>=a.selected.iter().filter(|s| q.options.iter().any(|o| &o.label==*s)).cloned().collect(); if !selected.is_empty() { self.selected.insert(q.id.clone(),selected); } if let Some(t)=&a.text && !t.trim().is_empty() { self.texts.insert(q.id.clone(),t.trim().into()); } } } if let Some(c)=draft.comment && !c.trim().is_empty() { self.comment=Some(c); } }
    pub fn answers(&self) -> QuestionAnswers { self.request.questions.iter().filter_map(|q| { let selected=self.selected_for(&q.id).to_vec(); let text=self.text_for(&q.id).map(str::to_owned); if selected.is_empty() && text.is_none() { None } else { Some((q.id.clone(),QuestionAnswer { selected,text })) } }).collect() }
    pub fn unanswered(&self) -> Vec<String> { self.request.questions.iter().filter(|q| !self.is_answered(&q.id)).map(|q| q.id.clone()).collect() }
    pub fn submit_outcome(&self,force_partial:bool) -> Option<QuestionStatus> { if self.comment.as_ref().is_some_and(|s| !s.trim().is_empty()) { Some(QuestionStatus::CommentSubmitted) } else if self.unanswered().is_empty() || self.answered_count()>0 || force_partial { Some(QuestionStatus::Answered) } else { None } }
    pub fn build_response(&self,status:QuestionStatus,auto_resolved_after_ms:Option<u64>) -> QuestionResponse { QuestionResponse { status, answers:self.answers(), comment:self.comment.clone().filter(|s| !s.trim().is_empty()), unanswered:self.unanswered(), auto_resolved_after_ms } }
    pub fn refresh_draft(&self) -> QuestionDraft { QuestionDraft { answers:self.answers(), comment:self.comment.clone().filter(|s| !s.trim().is_empty()) } }
}
