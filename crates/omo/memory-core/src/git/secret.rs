//! Commit-gate refusal for secret-like staged memory content (pin `git/secret.ts`,
//! `git/repo-secret-staging.ts`).

use std::fmt;

use crate::sync::redact::{
    SecretPatternClass, redact_secret_like_material, scan_secret_like_material,
};

use super::errors::GitError;
use super::exec::GitExecResult;

/// Where the secret-like material was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySecretLocation {
    Content,
    Path,
}

/// Refusal raised by the commit gate when staged memory content or a staged path is secret-like.
///
/// The message names the (masked) path and the pattern class only; the matched text never
/// appears in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySecretError {
    pub path: String,
    pub pattern_class: SecretPatternClass,
    pub location: MemorySecretLocation,
}

impl fmt::Display for MemorySecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = secret_pattern_class_name(self.pattern_class);
        match self.location {
            MemorySecretLocation::Path => write!(
                f,
                "refused: {} is a secret-like file name ({class})",
                redact_secret_like_material(&self.path)
            ),
            MemorySecretLocation::Content => write!(
                f,
                "refused: {} contains secret-like content ({class}); remove it and retry",
                self.path
            ),
        }
    }
}

impl std::error::Error for MemorySecretError {}

/// Upstream `SecretPatternClass` string literal for a class.
pub fn secret_pattern_class_name(class: SecretPatternClass) -> &'static str {
    match class {
        SecretPatternClass::AwsAccessKey => "aws_access_key",
        SecretPatternClass::CredentialAssignment => "credential_assignment",
        SecretPatternClass::AuthorizationHeader => "authorization_header",
        SecretPatternClass::OpenaiKey => "openai_key",
        SecretPatternClass::VendorToken => "vendor_token",
        SecretPatternClass::PemBlock => "pem_block",
        SecretPatternClass::SplitCredentialAssignment => "split_credential_assignment",
    }
}

/// First pattern class a scanner hit carries, or `None` when the text is clean.
pub fn secret_pattern_class_of(text: &str) -> Option<SecretPatternClass> {
    scan_secret_like_material(text)
        .first()
        .map(|found| found.class)
}

/// Refuse a staged commit that would carry secret-like material: first every normalized path
/// string, then every staged blob's full content. Reads come from the index (`git show :<path>`),
/// never the worktree, and deletions are skipped because their blob is gone.
pub fn assert_no_secret_like_staging(
    paths: &[String],
    mut git_result: impl FnMut(&[String]) -> Result<GitExecResult, GitError>,
) -> Result<(), GitError> {
    for path in paths {
        if let Some(pattern_class) = secret_pattern_class_of(path) {
            return Err(GitError::Secret(MemorySecretError {
                path: path.clone(),
                pattern_class,
                location: MemorySecretLocation::Path,
            }));
        }
    }
    for path in paths {
        let argv = ["show".to_string(), format!(":{path}")];
        let result = git_result(argv.as_slice())?;
        if result.code != 0 {
            continue;
        }
        if let Some(pattern_class) = secret_pattern_class_of(&result.stdout) {
            return Err(GitError::Secret(MemorySecretError {
                path: path.clone(),
                pattern_class,
                location: MemorySecretLocation::Content,
            }));
        }
    }
    Ok(())
}
