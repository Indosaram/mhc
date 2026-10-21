use crate::tool::types::EvalKernelRunInput;

/// Resolve source only when the queued cell owns its turn.
pub fn input_at_start(input: &EvalKernelRunInput) -> Result<(String, Option<String>), String> {
    match &input.resolve_at_start {
        Some(resolve) => resolve(),
        None => Ok((input.code.clone(), None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn source_is_read_at_resolution_not_construction() {
        let revision = Arc::new(AtomicUsize::new(1));
        let current = revision.clone();
        let input = EvalKernelRunInput {
            resolve_at_start: Some(Arc::new(move || {
                Ok((
                    current.load(Ordering::SeqCst).to_string(),
                    Some("/tmp/cell.js".into()),
                ))
            })),
            ..Default::default()
        };
        revision.store(2, Ordering::SeqCst);
        assert_eq!(
            input_at_start(&input),
            Ok(("2".into(), Some("/tmp/cell.js".into())))
        );
    }

    #[test]
    fn refused_source_does_not_fall_back_to_original_code() {
        let input = EvalKernelRunInput {
            code: "execute()".into(),
            resolve_at_start: Some(Arc::new(|| Err("unreadable".into()))),
            ..Default::default()
        };
        assert!(input_at_start(&input).is_err());
    }
}
