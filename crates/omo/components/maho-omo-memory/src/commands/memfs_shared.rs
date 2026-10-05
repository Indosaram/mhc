//! Shared plumbing between the /memfs dispatcher and its subcommand handlers.
//! Port of `components/memory/commands/memfs-shared.ts` at pin 77f3067f1.

use memory_core::git::GitMemoryRepo;

use super::args::ParsedCommandArgs;
use super::repo::{has_git_repo, open_repo};
use super::types::{
    BoxFuture, CommandContext, CommandResponse, MemoryCommandDeps, MemoryCommandIdentity,
};

pub struct MemfsSubcommandInput<'a> {
    pub deps: &'a MemoryCommandDeps,
    pub ctx: &'a CommandContext,
    pub parsed: &'a ParsedCommandArgs,
    pub identity: &'a MemoryCommandIdentity,
}

pub type MemfsSubcommand = fn(&MemfsSubcommandInput<'_>) -> BoxFuture<'_, CommandResponse>;

pub fn no_repo_text(identity: &MemoryCommandIdentity) -> String {
    format!(
        "no memory repository for {} at {}; run /memfs init first",
        identity.identity,
        identity.identity_paths.repo.display()
    )
}

/// Returns the repo, or the actionable error text when the repository does not exist.
pub fn require_existing_repo(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
) -> Result<GitMemoryRepo, String> {
    if !has_git_repo(identity) {
        return Err(no_repo_text(identity));
    }
    open_repo(deps, identity)
}
