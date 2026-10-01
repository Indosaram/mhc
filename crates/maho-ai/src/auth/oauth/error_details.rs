//! Port of senpi packages/ai/src/auth/oauth/error-details.ts.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ErrorDetails {
    pub name: String,
    pub message: String,
    pub code: Option<String>,
    pub errno: Option<String>,
    pub cause: Option<Box<ErrorDetails>>,
    pub stack: Option<String>,
}

impl ErrorDetails {
    pub fn format(&self) -> String {
        let mut details = vec![format!("{}: {}", self.name, self.message)];
        if let Some(code) = &self.code {
            details.push(format!("code={code}"));
        }
        if let Some(errno) = &self.errno {
            details.push(format!("errno={errno}"));
        }
        if let Some(cause) = &self.cause {
            details.push(format!("cause={}", cause.format()));
        }
        if let Some(stack) = &self.stack {
            details.push(format!("stack={stack}"));
        }
        details.join("; ")
    }
}

pub fn format_error_details(error: &(dyn std::error::Error + 'static)) -> String {
    details_of(error).format()
}

fn details_of(error: &(dyn std::error::Error + 'static)) -> ErrorDetails {
    let mut details = ErrorDetails { name: "Error".into(), message: error.to_string(), ..Default::default() };
    if let Some(io) = error.downcast_ref::<std::io::Error>()
        && let Some(code) = io.raw_os_error() {
            details.code = Some(code.to_string());
            details.errno = Some(code.to_string());
        }
    if let Some(source) = std::error::Error::source(error) {
        details.cause = Some(Box::new(details_of(source)));
    }
    details
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_error_formats_as_name_and_message() {
        let error = anyhow::anyhow!("boom");
        assert_eq!(format_error_details(error.as_ref()), "Error: boom");
    }

    #[test]
    fn io_error_carries_code_errno_and_nested_cause() {
        let error = std::io::Error::from_raw_os_error(48);
        let formatted = format_error_details(&error);
        assert!(formatted.starts_with("Error: "), "{formatted}");
        assert!(formatted.contains("code=48"), "{formatted}");
        assert!(formatted.contains("errno=48"), "{formatted}");
    }

    #[test]
    fn cause_chain_is_flattened_into_the_line() {
        #[derive(Debug)]
        struct Wrapper(std::io::Error);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "wrapper failed")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let formatted = format_error_details(&Wrapper(std::io::Error::from_raw_os_error(2)));
        assert!(formatted.starts_with("Error: wrapper failed; "), "{formatted}");
        assert!(formatted.contains("cause=Error: "), "{formatted}");
        assert!(formatted.contains("code=2"), "{formatted}");
    }
}
