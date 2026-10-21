# ULW input drift acceptance driver

Run after global CI consolidation, against the downloaded CI-built binary:

```sh
bun tools/qa/ulw-input-drift/driver.mjs --binary /absolute/path/to/mhc --out-dir /absolute/path/to/evidence/run-1
bun tools/qa/ulw-input-drift/driver.mjs --check-ferryx --out-dir /absolute/path/to/evidence/ferryx-probe
```

Requires Bun with native `Bun.Terminal` support (Linux/macOS), and the existing
`@xterm/headless` dependency. Resolution tries this repository, then
`$SENPI_SRC/package.json` (default `~/code/senpi`), then the existing Bun 6.0.0
cache location used by `tools/qa/parity-closure-tui-qa.mjs`, relative to the actual
user home or `BUN_INSTALL_CACHE_DIR`. No package installation or new dependency
is performed. That cache was present when this driver was authored. CI must
provide one of these resolution paths; missing dependencies are failures.

The driver creates a disposable HOME, agent directory, working directory and
runtime caches; it does not inherit user credentials or ambient agent/session
configuration. It uses a local ephemeral-port OpenAI-compatible SSE transport,
validates the submitted `ulw` request, and gates output on terminal observations.
It keeps native extensions enabled so it exercises the real ULW neon editor.
It disables tools, context files, skills and prompt templates for isolation.
It explicitly uses regular terminal mode (including scrollback), and leaves
hardware-cursor configuration at the binary's default rather than applying the
historical workaround to the fixture.

The scenario checks startup, typing `ulw drift-probe-7a91`, five distinct border
color frames, edit/backspace, submitted provider request, streamed text, 100x30
to 80x24 and back during streaming, five distinct frames at each resized layout,
final output and a usable post-output editor. Position is compared relative to
the viewport bottom within each held-layout phase, not across intentional
resize/reflow or transcript growth. Exactly one native `❯` input marker must
exist across viewport AND retained scrollback, enclosed by editor borders.
Checks observe completed synchronized-update frames, not partial PTY writes.
Every wait is an event predicate or process exit with a bounded failure deadline;
no fixed sleeps or polling. Distinct frames require changed border cell colors,
not repeatedly reading the same screen. Missing animation is a failure, not skip.

Stdout is JSON. `evidence.json` repeats it:

| Top-level status | Exit | Meaning |
| --- | --- | --- |
| `pass` | 0 | Local real-TUI PTY scenarios passed; Ferryx remains unverified |
| `fail` | 1 | Assertion, timeout, setup, argument, or teardown failure |
| `ferryx-unavailable` | 77 | `--check-ferryx` or `--require-ferryx`: no supported remote capture lane; PTY not run |

`ferryx.status` is always `ferryx-unavailable`, including on a PTY pass. There is
no environment-variable shortcut to remote verification. See
[MANUAL-FERRYX.md](MANUAL-FERRYX.md) for the separately recorded remote procedure.

Artifacts: per-scenario viewport and full-buffer text, raw `terminal.ansi`, the
actual `request.json`, dimensions, frame revisions, position anchors, binary
SHA-256 and teardown receipt in `evidence.json`. Use a fresh output directory per
invocation. Raw ANSI can be replayed for visual inspection; headless cell checks
are not screenshots or real Ferryx evidence. The driver does not claim an
agent-settled event merely because the last streamed token became visible.

## Claimed gap and implementation review

Native `tools/qa/` had only the three parity-closure drivers; the TUI one exercises
commands, not ULW animation/stream/resize invariants. Native interactive `.mjs`
goldens render fixtures, not this acceptance flow. Upstream omo has generic
browser-terminal and sidecar TUI drivers; upstream senpi has generic PTY driving
helpers. None supplies an equivalent native-binary ULW drift acceptance driver.
The misplaced drafts were reference material, not an existing worktree driver.

Corrected draft pitfalls: immediate reuse of a frame as five animation samples,
resize readiness based only on assigned dimensions, permitting zero editors,
ambient Ferryx hints promoted to verification, swallowed transport errors,
hardcoded user cache path and sleep-based teardown.

Implementation was reviewed by source inspection only. Per consolidation rules,
no tests, diagnostics, build, driver execution, commits or pushes were run in
this worker. Remaining validation: run the command above with the consolidated
CI binary, inspect artifacts and cleanup, exercise failure/timeout paths, replay
the ANSI visually, and perform separate actual Ferryx acceptance. No current
runtime PASS or regression fix is claimed by shipping this harness.
