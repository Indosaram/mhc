# Leak-check record: workflow-dag

Spec-writer self-check and independent reviewer record for docs/cleanroom/specs/workflow-dag.md. Reviewer input only; not an implementer input.

## Leak self-check

- [x] no fenced code blocks; no SUL code, struct layouts or comments reproduced;
- [x] tool parameter descriptions and messages are written new; no prompt text or schema
      description sentences are copied or paraphrased;
- [x] enumerations (actions, states, error codes, config keys) are marked interface facts;
- [x] proposed identifiers are neutral (`Workflow*`, `workflows/`); no OmO persona names;
- [x] OmO-original features (magic-keyword orchestration, mass fan-out mode, persona prompts,
      plan-continuation state file, the node-prompt marker contract) are not specified.

## Review (leak check)

Reviewer session: separate from the spec writer. Compared against the SUL workflow runtime, its
tool description and parameter schema, the `/dag` command, and its code and state tables.

- [x] no fenced code block is Rust, TypeScript or JS from the SUL tree (the file has no fenced blocks);
- [x] no sentence is a translation or paraphrase of a SUL comment, doc comment or prompt (after the fixes below);
- [x] no list or table mirrors a SUL constant table except interface facts (run/node states, node and
      tool error codes, validation codes, `action` values, result `kind` values, config keys - all
      appear in tool results, snapshots or `/dag` output a caller sees);
- [x] no proposed identifier is an OmO-original name (`Workflow*`; `workflow`, `/dag`, `task.dag.*` kept as interface facts);
- [x] OmO-original features are absent.

Fixes:
- §1: the edge/prompt sentence closely paraphrased the SUL tool description; it now gives the same
  fact in different terms.
- §1.1: the advisory warning example (a graph without a verification node) was taken from a SUL
  warning message tied to the OmO node-prompt convention. Removed it; warnings are now implementer-defined.
- §4 table: the `attach`, `retry`, `send`, `amend`, `guide`, `cancel_node` and `resume` rows followed
  the sentence-by-sentence wording of the SUL `action` parameter description. Rewrote them as
  state-transition facts.
- §4: the open-ended result `kind` list (`...`) was completed with the user-visible values `retried`, `sent`, `amended`
  (accuracy fix, not a leak).
