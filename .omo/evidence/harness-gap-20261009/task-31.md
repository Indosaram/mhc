# Task 31 - integrate, build and stage native package

Source: integration/harness-gap-20261009 @ dc53618f20021bd5ea55f389f2a1435fb12560b1, tree 97e3ca40556f6fb5c1661a9e908574cc195d475a.

## Gates on this source
| Gate | Command | Result |
|---|---|---|
| Workspace tests | `cargo nextest run --workspace --no-fail-fast` | 20231/20231 passed, 23 skipped (env-dependent live desktop N/A, see skipped-live-tests.md) - workspace-gate-9.txt |
| Clippy | `cargo clippy --workspace --all-targets --keep-going -- -D warnings` | exit 0 - clippy-final.log |
| Parity | `bun tools/parity-audit.mjs --all` | 138 PASS, 0 FAIL |
| Packaging self-test | `bun tools/package-native.mjs --self-test` | 91/91 passed, exit 0 |

## Release build
`cargo build --release --workspace --bins` exit 0 in 8m24s (release-build.txt).

## Staged package
`bun tools/package-native.mjs --binary <release>/mhc --output install --skill-source latest --git-bash-binary <release>/omo-git-bash --lsp-daemon-binary <release>/lsp-daemon --desktop-engine-binary <release>/senpi-desktop-engine --ast-grep-mcp <release>/ast-grep-mcp --omowright-runtime <materialized omowright 0.0.1 @293ca5002cbd4c8b0c104c5934683385ef7e0d3a>`
exit 0: 5 executables, 25 skills (337 files), 2 licenses (package-stage.log).

**mhc sha256: `b0088a1a4449e3b10d5747d60ecd64c77ea0fbede9538266f98243931e06c50a`** (staged == built).

## QA scenarios (package-qa.log)
| Scenario | Observable | Result |
|---|---|---|
| Untampered `--verify-only install` | exit 0, "verified (346 files)" | PASS |
| Tampered sibling: `lsp-daemon` +1 byte | exit 1, "size mismatch" + "sha256 mismatch: lsp-daemon" | PASS (rejected) |
| Missing sibling: `omo-git-bash` removed | exit 1, "missing: omo-git-bash" | PASS (rejected) |

## Cleanup receipt
- tamper/missing copies: `rm -rf /tmp/pkgqa.k3rPom` (verified absent).
- materialized omowright runtime /tmp/omowright-runtime.MkQMpT: removed after staging (bytes are hashed into install/manifest.json).
