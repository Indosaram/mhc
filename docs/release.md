# Releasing mhc

One release = three targets. `scripts/release-all.sh <version>` builds, signs, and
publishes them; this document explains the parts that have gone wrong before.

## Release matrix

| target | host | script |
|---|---|---|
| `linux-x86_64` | this machine | `scripts/release-linux.sh` |
| `windows-x86_64` | `win-new` | `scripts/release-windows.ps1` |
| `macos-aarch64` | `maho-mac` | `scripts/release-macos.sh` |

Never build or publish `linux-aarch64` or `macos-x86_64`. They are not release
targets even though `.github/workflows/release.yml` lists them. Check the previous
release's asset list before building.

Each target ships two profiles: the default (`mhc-<target>`) and the MIT profile
(`mhc-<target>-mit`, built with `--no-default-features`). Each per-target script
builds both; set `MHC_RELEASE_SKIP_MIT=1` (or `-SkipMit` on Windows) to build
only the default profile. Asset names carry no version so
`releases/latest/download/<name>` always resolves.

## Versioning

A same-day follow-up release appends `.1` (`v2026.11.12` then `v2026.11.12.1`),
following the `v2026.10.25.1` precedent. `MHC_BUILD_VERSION` carries it in.

## Host traps

These have each cost a release round trip. The scripts encode the fixes; read
this before hand-running anything.

### All hosts: the stale target directory

`MHC_BUILD_VERSION` only feeds the version string. If `CARGO_TARGET_DIR` already
holds a build of the same source, cargo considers the crate fresh and never
re-runs, so packaging happily archives the *previous* release's binary under the
new name. Every build stage therefore asserts `mhc --version` before packaging.
If the assertion fails, clear the target directory — do not "fix" it by bumping
the version by hand.

### Windows: the SSH shell is already PowerShell

`ssh win-new` lands in **PowerShell 7**, not cmd. Wrapping a build in
`cmd /c "..."` or appending `> log 2>&1` therefore gets parsed by PowerShell,
which either rewrites the arguments into something else or dies on them. Every
attempt to launch the script that way hung silently with a truncated log and no
process visible. Invoke `pwsh -NoProfile -ExecutionPolicy Bypass -File
scripts/release-windows.ps1 ...` directly over SSH and let the script do its own
logging.

### Windows: protoc is not on the SSH PATH

`maho-ai/build.rs` compiles vendored protobuf schemas and needs `protoc`. It is
installed at `C:\Users\sook\build\protoc\bin\protoc.exe` but an SSH session does
not inherit it, so the build dies with `Could not find protoc`. Pass
`-ProtocPath` or set `PROTOC`; `release-windows.ps1` also prepends its directory
to `PATH`.

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

### macOS: maho-mac over the Ferryx relay

`maho-mac` (`100.65.239.35`) is often offline on the tailnet. When direct SSH
times out, do **not** substitute another machine and do not ask which host to
use — route through the Ferryx relay from the user's MacBook (`ssh mac`):

```
/Applications/Ferryx.app/Contents/MacOS/ferryx-cli agents list \
  --host https://relay.ferryx.dev/host/36b219b4-88e1-4612-9ac2-d41bd4830064 --json
```

The host id and label `maho-mac` are in `~/.ferryx/remote/paired-hosts.v1.json`.
Panes need their **full** session UUID; a truncated id returns `NOT_FOUND`.

Every pane on maho-mac is an omo agent, not a shell, so `agents send` types into
an agent's prompt and it spends a turn on the request. Two consequences: phrase
the instruction transparently (naming the repo, the script, and that nothing is
uploaded) rather than pasting an opaque blob, and never send a command expecting
shell semantics. `agents stop` aborts a turn that has gone wrong.

`ferryx-cli file cp` resolves its source path on the MacBook where the CLI runs,
not locally, and can return `PAIRED_HOST_CAPABILITY_UNAVAILABLE`.

### All hosts: run the scripts from inside the repo

`release-windows.ps1` reads `assets/omo-latest/manifest.json` relative to the
repository root, so invoking a copy that lives outside the checkout fails on the
manifest lookup. It now resolves the manifest to an absolute path and reports
when it is missing.

## Signing and publishing

The real minisign key lives at `~/.config/mhc-release/mhc-release.key` with its
password beside it; the public half is published as `mhc-release.pub`. Sign each
`.tar.gz` and `.zip` (`.minisig`), write a `.sha256` per asset, upload everything
to a draft release, then publish it. Re-running for an existing tag replaces the
assets: `gh workflow run release -f tag=<tag>`.

## After the release

Remove the per-run source copies, staging dirs, and logs from every host used.
Shared compile caches (`mhc-release-target`) stay unless disk is short.
