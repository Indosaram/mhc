//! Port of senpi `packages/coding-agent/src/cli/args.ts`.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliPromptSizeArgs {
    pub print_prompt_sizes: bool,
}
