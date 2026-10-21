# Ferryx acceptance: separate, operator-observed evidence

The local driver never verifies Ferryx, even when launched from a Ferryx shell.
Its child output terminates in a private headless xterm, not the remote client.
No supported Ferryx attach command, environment probe, socket location, or
automated capture interface was established by the available evidence. Use your
deployment's documented session creation/attachment procedure; do not treat
`FERRYX_*` variables or a file named like a socket as proof.

## Evidence boundary

The available historical record is
`/home/projects/mhc/.omo/evidence/render-compare/FINDINGS.md` (2026-10-13).
The corresponding worktree-local file was absent at authoring time.
It compares 54x35 PTY captures with a gated offline provider. It reports idle
redraws, 18.2 fps / 44237 B/s with `ulw` typed, and missing cursor restoration
in the recorded native frames versus explicit row/column restoration in senpi.
It attributes the latter to hardware-cursor configuration. These are historical
capture findings, not proof about the current CI binary or Ferryx internals.
The record does NOT establish broadcast capacities, dropped packets, daemon
implementation, or a particular remote VT backend. Do not claim those mechanisms.

## Operator procedure

1. Record the exact CI artifact identifier and `sha256sum /absolute/path/to/mhc`.
   Attach to an actual Ferryx remote session using your installed version's
   documented procedure. Record client/daemon versions and the actual command.
2. Launch that binary directly in the remote terminal, using a disposable HOME,
   `MAHO_CODING_AGENT_DIR`, and working directory. Preconfigure its provider and
   onboarding from your verified setup. Do not point it at private production
   state. The automated driver's private local transport is not a service for
   this manual session; use a separately controlled provider, or record that
   streamed-response acceptance remains unverified.
3. At 100x30, type `ulw drift-probe-7a91` without Enter. Capture a screen recording
   of multiple visibly distinct border animation frames. Add a character,
   backspace it, and retype. At unchanged dimensions, the single editor must
   retain its row; there must be no stamped copies in scrollback.
4. Resize to 80x24, then 100x30. Capture before/after dimensions and screenshots.
   Reflow may move the editor during resize, but subsequent animation frames
   must hold position at each size. Exactly one editor remains.
5. Submit with Enter. Record the provider request and deliberately gated streamed
   output, resize during streaming, then complete the response. Verify one usable
   editor after completion and no orphan input copies in viewport or scrollback.
6. Close the disposable process/provider and remove only its sandbox. Record exit
   and removal receipts. Preserve recordings and artifact hashes outside it.

Record a separate JSON document with `surface: "ferryx-remote"`, `status: "pass"`
or `"fail"`, binary hash, actual attachment command, session identity, dimensions,
provider setup, observations for every step, recording/screenshot paths, operator,
timestamp, and cleanup receipts. Missing transport or capture evidence means
`status: "ferryx-unavailable"`, not pass. Do not overwrite or upgrade the local
driver's `evidence.json`. A plain PTY result is never Ferryx signoff.
