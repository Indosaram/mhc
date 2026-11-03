# Leak-check record: task D2/D4 clarifications (omo v5.1.52)

Scope: spec commit 46dfb7d06 on sync5152/spec. It adds "Matching rules (clarified
2026-11-01)" and tests 10-11 to task.md D2, and the queued-message clause plus test 7 to
task.md D4. Reviewer: a separate session, not the spec writer. This record is for reviewers
only; implementers must not use it as input.

Compared against: upstream omo v5.1.52
`packages/senpi-task/src/manager/credential-failure.ts` and `credential-failure.test.ts` (diff
base 0a401eb8c8); task-control.md §1 (pending list merged into the first run).

## Checklist (README rules 1-6)

- [x] No fenced code blocks.
- [x] No translated SUL comment. The upstream comments explain why a `/login` hint alone is not
      enough and list unrelated texts that contain "expired". The spec's reason (the input
      includes the child's log tail) is mhc's own. The sample strings in tests 10-11 are new
      and invented. None of the upstream negative examples (device-authorization expiry, MCP
      session expiry, prompt-cache expiry) is copied.
- [x] No row-for-row table. The words listed (account, key, token, credential, organization,
      subscription) restate the D2 403 rule that was already reviewed. They are given as
      whole-word rules, which is mhc's own choice: upstream matches substrings. The regular
      expressions are not reproduced.
- [x] No OmO-original identifier.
- [x] No OmO-original feature.

## Behaviour check

- Whole-word matching with an optional plural "s": consistent with tests 7 and 10. "keyword"
  and "monkey" do not match. Upstream would not match them either, since its key word requires
  an "api" prefix, so there is no conflict.
- Expiry rule: **tightened.** As written ("the same text also mentions `/login`"), an unrelated
  log-tail line "... session expired" together with another line naming `/login` (for example
  the engine's "No model selected" help) would be classified `rejected`. That contradicts the
  rule's own reason and the D2 bullet that a `/login` mention alone is not evidence. The rule
  now requires the `/login` instruction to be the sentence that directly follows the expiry
  statement. Test 6 ("Meta session expired (status 401). Run `/login meta` ...") still holds,
  and test 11 gained a case where the `/login` sentence does not directly follow.
- D4 queued-message clause: consistent with task-control.md §1 (queued messages are merged into
  the first run's prompt). The clause was inserted inside the parenthetical about the fresh
  session id, which made the sentence ambiguous. It was moved after `runs` stays 1, with a
  cross-reference to task-control.md §1. Test 7 agrees with the text.

## Fixes made

1. Expiry rule: the `/login` instruction must directly follow the expiry statement. Test 11
   extended.
2. D4: re-ordered the queued-message clause out of the session-id parenthetical and added the
   task-control.md §1 reference.

verdict: approved
