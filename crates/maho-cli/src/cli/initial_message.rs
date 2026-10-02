use super::args::Args;
use maho_ai::types::ImageContent;
pub struct InitialMessageResult { pub initial_message: Option<String>, pub initial_images: Option<Vec<ImageContent>>, pub initial_title_prompt: Option<String> }
pub fn build_initial_message(parsed: &mut Args, file_text: Option<&str>, file_images: Option<Vec<ImageContent>>, stdin_content: Option<&str>) -> InitialMessageResult {
    let mut parts = Vec::new();
    if let Some(stdin) = stdin_content { parts.push(stdin.to_owned()); }
    if let Some(file) = file_text.filter(|s| !s.is_empty()) { parts.push(file.to_owned()); }
    let private = stdin_content.is_some() || file_text.is_some() || file_images.as_ref().is_some_and(|images| !images.is_empty());
    let title = if private { None } else { parsed.messages.first().cloned() };
    if !parsed.messages.is_empty() { parts.push(parsed.messages.remove(0)); }
    InitialMessageResult { initial_message: (!parts.is_empty()).then(|| parts.join("")), initial_images: file_images.filter(|images| !images.is_empty()), initial_title_prompt: title }
}
