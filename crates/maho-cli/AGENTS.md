# crates/maho-cli

## OVERVIEW
Primary user-facing command-line interface entry points, argument parsing, and migration runners.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| CLI command routing | `src/cli/` | Subcommand dispatch (auth, import-omo, models discover, install/remove/uninstall/update/list, schedule, doctor, host, config, mini) |
| Configuration loading | `src/config.rs` | Local and global `.omo` config discovery |
| Config discovery hierarchy | `src/nearest_parent_config.rs` | Walking upward to find closest repo configurations |
| Migrations | `src/migrations_state.rs` | Migration state tracking and status inspection |
| Brand & directory migrations | `src/brand_dir_migration.rs` | Migrating legacy directory structures to `.omo` |
| Extension migrations | `src/extension_system_migration.rs` | Native extension metadata verification |
| Working directory validation | `src/valid_cwd.rs` | CWD verification and path normalization |

## CONVENTIONS
- CLI commands must exit with clean semantic exit codes.
- Arguments are parsed by hand in `src/cli/args.rs` and `src/package_manager_cli.rs`; there is no clap dependency.
- Maintain backwards compatibility for legacy invocation flags and options.
- Workspace lints enforced: zero unwrap in application setup.

## ANTI-PATTERNS (THIS COMPONENT)
- Avoid interactive prompts when running in non-interactive/CI modes (`--non-interactive`).
- Do not bypass error reporting formatters on fatal startup failures.
- Never write output directly to stdout when JSON/quiet mode is requested.

## NOTES
- Scored 15 due to module boundary responsibility as the primary binary entry point.
- Global config directory resolves to `~/.omo/agent` while project state remains in `.omo`.
