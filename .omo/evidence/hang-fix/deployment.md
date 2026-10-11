# Deployment receipt

Final workspace release build: exit 0 (`bash_123`, release-final.log). Package staging exited 0 and verify-only validated 345 files.

Installed package: `/home/indo/.local/share/mhc`. Invocation link: `/home/indo/.local/bin/mhc` -> `/home/indo/.local/share/mhc/mhc`. Previous installation retained at `/home/indo/.local/share/mhc-pre-hang-fix-20261012`.

Both installed paths have SHA256 `e3ee2d468eeb31e7ab1f974ecb73471492d3a92c5e5615228bd345d14eaabb82`.

Installed doctor exited 0 and reported PASS for project resources and all four native executables. It also reports the existing user configuration warning that architect has no connected provider; this does not prevent binary startup and was not changed by this fix. Complete output: installed-doctor.log.

Real rebuilt package PTY QA passed: actual read result observed by the provider, final response, responsive second turn, component warning present in hidden-debug.log and absent from terminal-ansi.txt. Receipt confirms PTY exited, provider closed and temp HOME removed. Five-crate validation passed 2810 tests, 0 skipped; strict all-target clippy exited 0. Baseline regression failed with the requested two-second renderer snapshot timeout.
