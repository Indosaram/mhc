use super::tool_context::{PostMutateContext, PostMutateHook};
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostMutateOutcome {
    pub file_may_have_changed: bool,
    pub note: Option<String>,
}
pub async fn run_post_mutate(
    hook: Option<&PostMutateHook>,
    input: PostMutateContext,
) -> PostMutateOutcome {
    let Some(hook) = hook else {
        return PostMutateOutcome::default();
    };
    match hook(input).await {
        Ok(result) => PostMutateOutcome {
            file_may_have_changed: result.changed,
            note: result.note,
        },
        Err(error) => PostMutateOutcome {
            file_may_have_changed: true,
            note: Some(format!("postMutate hook failed: {error}")),
        },
    }
}
pub fn append_post_mutate_note(text: &str, notes: &[Option<String>]) -> String {
    std::iter::once(text)
        .chain(notes.iter().filter_map(Option::as_deref))
        .collect::<Vec<_>>()
        .join("\n")
}
