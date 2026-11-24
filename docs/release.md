# Releasing mhc

One release = three targets. `scripts/release-all.sh <version>` builds, signs, and
publishes them; this document explains the parts that have gone wrong before.

## Release matrix

| target | built on | script | driver setting |
|---|---|---|---|
| `linux-x86_64` | this machine | `scripts/release-linux.sh` | none |
| `windows-x86_64` | the Windows builder, over ssh | `scripts/release-windows.ps1` | `MHC_WIN_HOST` |
| `macos-aarch64` | the Apple Silicon builder, over ssh | `scripts/release-macos.sh` | `MHC_MAC_HOST` |

Never build or publish `linux-aarch64` or `macos-x86_64`. They are not release
targets even though `.github/workflows/release.yml` lists them. Check the previous
release's asset list before building.

Each target ships two profiles: the default (`mhc-<target>`) and the MIT profile
(`mhc-<target>-mit`, built with `--no-default-features`). Each per-target script
builds both; set `MHC_RELEASE_SKIP_MIT=1` (or `-SkipMit` on Windows) to build only
the default profile. `release-all.sh` forwards that switch to all three targets, so
a run cannot ship the MIT profile from two hosts and omit it from the third. Asset
names carry no version so `releases/latest/download/<name>` always resolves.

## Configuration

Host names, host paths and ssh details are deliberately not tracked here (see
`AGENTS.md`: the private build-host note is untracked local state). The driver reads
them from the environment, or from a file you name with `MHC_RELEASE_ENV`, which is
sourced when set. Keep that file outside the repository, or inside a gitignored
local directory, so the real values cannot be committed.

| variable | meaning |
|---|---|
| `MHC_WIN_HOST` | **required**; ssh destination of the Windows builder |
| `MHC_MAC_HOST` | **required**; ssh destination of the Apple Silicon builder |
| `MHC_WIN_BASE` | base directory on the Windows builder; default `%USERPROFILE%/build` |
| `MHC_MAC_BASE` | base directory on the mac; default `$HOME/build` |
| `MHC_WIN_PROTOC` | `protoc.exe` on the Windows builder; forwarded as `-ProtocPath` |
| `MHC_RELEASE_OUT` | parent of this run's asset directory; default `~/.cache/mhc-release` |
| `MHC_RELEASE_TARGET_DIR` | shared cargo cache; default `<MHC_RELEASE_CACHE>/target` |
| `MHC_RELEASE_TMPDIR` | scratch space on this machine; default `$TMPDIR`, then `/var/tmp` |
| `MHC_RELEASE_CACHE` | parent of the reusable Linux cargo cache; default `~/.cache/mhc-release-build` |
| `MHC_RELEASE_KEY`, `MHC_RELEASE_PUBKEY` | minisign private and public key |
| `MHC_RELEASE_SKIP_MIT` | `1` builds only the default profile, on every target |
| `MHC_RELEASE_ENV` | file holding the above; sourced when set |

The driver itself needs `bash`, `git`, `node`, `bun`, `gh`, `minisign`, `ssh` and
`scp`. `MHC_WIN_BASE` and `MHC_MAC_BASE` must not contain spaces: the assets are
collected with scp, which passes its remote path through verbatim.

### Output is never a directory the operator already owns

`MHC_RELEASE_OUT` names the *parent* of the run directory, not the output directory
itself. The driver makes one `mktemp -d` directory under it, writes everything
there, and removes only that directory after publishing. A release therefore never
deletes unrelated files that happen to sit in a directory the operator named, and
`--build-only` keeps the run directory and prints its path instead.

## One commit per release

All three targets are built from one commit, and nothing else in this checkout can
reach an artifact:

- The driver requires `main` with `HEAD == origin/main` and records that commit. A
  working tree with uncommitted or untracked changes is reported and otherwise
  ignored: the release is the commit, not the checkout.
- Linux used to build straight from the working tree, so staged-but-uncommitted
  files could ship from one target and not from the other two. It now fetches the
  recorded SHA into an empty `linux-src` under the unique run directory, checks that
  checkout's `HEAD`, and builds there. The fetch is by commit, not by branch, so the
  checkout cannot hold anything the commit does not.
- The Windows and mac builders fetch the recorded commit, run `git clean -qdf`, and
  only then confirm `git rev-parse HEAD`, so neither a leftover untracked file from an
  earlier run nor a checkout parked on another commit can reach an artifact.
- The omowright pin is read from the commit itself
  (`git show <commit>:assets/omo-latest/manifest.json`), not from the working tree,
  so the manifest that ships is the manifest that was committed.
- `CARGO_TARGET_DIR` is pinned to the shared cache (`MHC_RELEASE_TARGET_DIR`, default
  under `MHC_RELEASE_CACHE`, relative paths taken from the repository root) before the
  Linux checkout builds. It survives the removal of the run directory.

## Versioning

A same-day follow-up release appends `.1` (`v2026.11.12` then `v2026.11.12.1`),
following the `v2026.10.25.1` precedent. `MHC_BUILD_VERSION` carries it in.

## Tags

A release tag must point at the commit that was built.

- Before building, and again before publishing, the driver reads `refs/tags/v<version>`
  from origin with `git ls-remote` (a `--build-only` run does neither). A failed lookup
  stops the run: mistaking an unreachable remote for "no tag" would create the tag at
  whatever `main` holds when the upload finishes. An annotated tag is compared by its
  peeled commit.
- An existing tag on another commit stops the run. The assets would otherwise be
  attached to different sources; take the next version instead, and do not move a
  published tag.
- A tag that does not exist yet is created by `gh release create` with
  `--target <built commit>`. Without `--target`, gh creates the tag from the latest
  state of the default branch, which can move while the build runs. An existing tag
  is passed `--verify-tag` instead, so gh can neither create nor move one.
- After publishing, the driver reads the tag's commit back and fails the run if it
  is not the built commit.

## Host traps

These have each cost a release round trip. The scripts encode the fixes; read this
before hand-running anything.

### All hosts: the stale target directory

`MHC_BUILD_VERSION` only feeds the version string. If `CARGO_TARGET_DIR` already
holds a build of the same source, cargo considers the crate fresh and never
re-runs, so packaging happily archives the *previous* release's binary under the
new name. Every build stage therefore asserts `mhc --version` before packaging.
If the assertion fails, clear the target directory — do not "fix" it by bumping
the version by hand.

### Windows: do not rely on the SSH login shell

A Windows ssh account may land in cmd.exe, in Windows PowerShell 5.1, or in pwsh,
depending on `DefaultShell` in `sshd_config`. Assuming PowerShell is what broke the
driver: it sent `'$env:USERPROFILE'` and a `pwsh ... -File` command line, and on a host
whose login shell is cmd the variable was never expanded and the quoting was mangled.

Pass everything as `powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand
<base64>`, where `<base64>` is the script encoded as UTF-16LE (see `win_ps` in
`scripts/release-all.sh`). The login shell only has to hand one opaque argument to
`powershell`, so neither the quoting nor the variable syntax depends on what that
shell is. Wrapping a build in `cmd /c "..."` or appending `> log 2>&1` still gets
parsed by whatever shell is active and mangles the arguments; let
`scripts/release-windows.ps1` do its own logging instead.

The checkout preparation, the `git rev-parse HEAD` check and the cleanup travel the
same way (`Test-Path`, `New-Item -Force`, `git -C`, `Remove-Item -Recurse -Force`).
cmd.exe's `if exist`, `cd /d` and `rmdir /s /q` fail there before the build script is
ever reached.

### Windows: protoc is not on the SSH PATH

`maho-ai/build.rs` compiles vendored protobuf schemas and needs `protoc`. It is
installed on the builder, but an SSH session does not inherit the PATH entry, so the
build dies with `Could not find protoc`. Point `MHC_WIN_PROTOC` at the executable
(the driver forwards `-ProtocPath`), pass `-ProtocPath` when running the script by
hand, or set `PROTOC`; `release-windows.ps1` also prepends its directory to `PATH`.

### Windows: PowerShell error handling

`$ErrorActionPreference = "Stop"` does not observe native exit codes, and cargo
writes ordinary progress ("Downloading crates ...") to stderr — under `Stop`
PowerShell treats that progress as a fatal error and kills the build mid-flight.
`release-windows.ps1` uses `Continue` plus an explicit `$LASTEXITCODE` check
after every step.

### Windows: schtasks does not launch the build

`schtasks /run` reports success while the task stays in "Ready" and never
starts; no work directory appears. Drive the build over SSH directly.

### macOS: the executable bit is not committed

`scripts/release-macos.sh` was committed as mode `100644`, so a fresh clone has
it non-executable. Invoking it as `./scripts/release-macos.sh` fails, and because
the driver lacked `set -e` the failure was silent: the MIT profile still built
while the SUL archive was never produced. The mode is now `100755`, and callers
invoke `bash scripts/release-*.sh` so a lost bit can never break a build again.

### macOS: the builder is often unreachable

The Apple Silicon builder is frequently offline. When direct SSH times out, do
**not** substitute another machine, do not invent a replacement target, and do not
ask which host to use. The alternate routes to that machine — relay or remote-session
tooling, its identifiers, and the rule that its panes are agents rather than shells —
are the operator's own business and belong in the untracked private note `AGENTS.md`
names, never in this repository. The driver stops with an explicit error naming the
environment variable to set.

### All hosts: run the scripts from inside the repo

`release-windows.ps1` reads `assets/omo-latest/manifest.json` relative to the
repository root, so invoking a copy that lives outside the checkout fails on the
manifest lookup. It now resolves the manifest to an absolute path and reports
when it is missing. The driver always runs the scripts from the checkout of the
built commit.

## Signing and publishing

The real minisign key lives at `~/.config/mhc-release/mhc-release.key` with its
password beside it; the public half is published as `mhc-release.pub`. Sign each
`.tar.gz` and `.zip` (`.minisig`), write a `.sha256` per asset, upload everything
to a draft release, then publish it. Re-running for an existing tag replaces the
assets: `gh workflow run release -f tag=<tag>`. `release-all.sh` itself only accepts
an existing tag that already points at the commit it built.

## After the release

`release-all.sh` removes the run directory it created, which holds the clean Linux clone,
and the per-run source copies and staging directories on both remote hosts. Shared compile
caches stay unless disk is short. A `--build-only` run keeps its run directory and its clone
and prints where they are; delete those by hand. Remote trees use `rel-` followed by the
unique run directory name under `MHC_WIN_BASE` and `MHC_MAC_BASE`. After a failure during
publishing, check the release and tag before retrying.
