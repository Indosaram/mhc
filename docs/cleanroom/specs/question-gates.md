# Approval question gates — behavior spec (Phase 5)

Scope: the small rule layer that sits in front of the structured ask-user question tool and
makes approval questions blocking. The question tool itself (its two parameter variants, the
pending-answer registry, timeouts, rendering, resume of dangling questions) is already MIT code
(`crates/builtins/maho-ext-ask-user` and the senpi `askUser` settings in `maho-core`) and is
**not** re-specified here; §1 only restates the parts of its public surface this unit depends on.
Proposed neutral name for the new crate/extension: `maho-approval-gates` (extension id
`approval-gates`).

## 1. Question tool surface this unit relies on (interface facts, MIT)

Two tool names carry the same feature for different model families:

| Tool | Questions | Options per question | Wait flag |
| --- | --- | --- | --- |
| `ask_user_question` | 1-4 | 2-4 | `waitForAnswer` (required) |
| `request_user_input` | 1-3 | 2-3 | `wait_for_answer` (required) |

- Each question has `question` (non-empty text), `header` (1-12 characters, shown as a chip),
  `options[]` of `{label, description}`, and `multiSelect` (boolean; `request_user_input` also
  carries a snake_case `id`). A free-form "Other" answer is always offered by the client, so
  callers do not list one.
- Wait flag `true`: the tool call blocks until the user answers or the ask-user timeout
  (`askUser.timeoutMinutes` in senpi settings, default 30, clamped 1-120) runs out. `false`:
  the call returns at once and the answer arrives later as a user message (deferred answer).
- `required` (optional boolean, default false): the answer decides whether an action is taken.
  When a required question gets no answer (timeout, dismissal), the tool result tells the model
  that the gated action must not be taken; a non-required unanswered question lets the model
  continue on its own judgement.
- `askUser.enabled: false` disables the tool family; this unit then has nothing to act on.

## 2. What this unit adds

A tool-call interceptor that runs before the question tool executes. When a call to either
tool above looks like an approval gate and `required` is not already `true`, the interceptor
sets `required` to `true` in the call input and lets the call proceed. It never blocks, rejects,
or rewrites any other field, and it never changes which tool runs.

### 2.1 Gate detection (in this order, first hit wins)

1. **Header rule.** Any question whose `header`, after trimming leading/trailing whitespace
   with the same character set as JavaScript `String.prototype.trim` (so U+FEFF is trimmed and
   U+0085 is not), equals one of these values case-insensitively: `Approval`, `Authorize`,
   `Authorise`, `승인`, `권한`. All questions are checked for this rule before the label rule is
   tried.
2. **Label rule.** Otherwise, any option whose trimmed `label` starts with `approve`,
   `authorize` or `authorise` (ASCII case-insensitive) and where the next character is not an
   ASCII letter (end of label, space, punctuation, digit and non-ASCII letters all count as a
   boundary).

Only `header` and option `label` values are examined. A header matches only when it is exactly
one of the listed words after trimming; a label matches only when the word is its first word.

The header values above are an interface fact: other components that create approval questions
use them, and each is within the 12-character header limit.

### 2.2 Input robustness

Inputs that are not objects, a missing or non-array `questions`, non-object question or option
entries, and non-string `header`/`label` values are skipped silently; the input is left
untouched. Non-tool-call events are ignored.

### 2.3 Diagnostics

When a gate is forced, the unit may write one debug-level line carrying only: the tool name,
the rule kind (`header` or `label`) and the matched question's trimmed header if it has one.
It must not log question text, option labels or descriptions.

## 3. Configuration and lifecycle

No config keys of its own. The extension is stateless, registered once per session, and active
whenever the question tools are registered. It is mounted in the MIT profile alongside the
ask-user builtin; disabling it is done the same way as any other bundled extension.

## 4. Platform notes

None; pure input transformation, identical on all platforms.

## 5. Acceptance tests

1. `ask_user_question` with header `Authorize`, wait flag false and `required` absent: after the
   interceptor, `required` is `true`; with `required: false` it also becomes `true`.
2. Each of `AUTHORIZE`, `Authorise`, `approval`, `승인`, `권한` (with a leading tab, trailing
   spaces or U+FEFF) as a header forces `required`; the diagnostic records rule `header` and the
   trimmed header.
3. Header rule beats label rule: question 1 has header `Deploy` with option `Authorize deploy`,
   question 2 has header `\tAuthorize`; the result reports rule `header`, header `Authorize`.
4. `request_user_input` with header `Release` and option label `authorize-release` forces
   `required`; the diagnostic records rule `label`, header `Release`.
5. Labels `approve`, `Authorise now`, `APPROVE: all`, `approve2`, `Authorizeü` match;
   `Approval`, `Approving`, `Authorised`, `Unapprove`, `Please authorize` do not.
6. Prose only: header `Plan check`, question text and descriptions mentioning "authorize",
   options without a matching label: input unchanged byte for byte, no diagnostic. Headers
   `Approval needed`, `승인 요청`, `Reapproval` do not match.
7. Header `\u0085Authorize` does not match (U+0085 is not trimmed).
8. A gate call that already has `required: true` is unchanged and produces no diagnostic.
9. A gate-shaped input sent to an unrelated tool (e.g. `write`) is unchanged.
10. Malformed inputs (`null`, `"text"`, `0`, `{"questions": null}`, `{"questions": "x"}`,
    questions containing booleans/strings, a numeric header or an object label) are unchanged
    and do not error.
11. A label-rule match on a question without a header produces a diagnostic with no header field.
12. The diagnostic for a forced gate contains none of the question text or option strings.
13. Each listed header passes the `ask_user_question` schema; a header
    longer than 12 characters (e.g. `Approval gate`) is rejected by the schema.
14. End to end: a forced-required question that times out yields the "do not take the action"
    result from the ask-user builtin, while the same question without a gate header and label
    yields the ordinary unanswered result.
