# Verification resource recovery

The original bash_1 batch completed all 33 parity commands and the boulder-state nextest run (93 passed, 0 skipped), then was terminated during maho-cli test compilation. The comment-checker-core parity count format was repaired and its audit rerun passed with exit code 0.

The first bounded continuation used two Cargo jobs and the existing remaining-maho-cli target. maho-cli test binaries migrations and duration failed during linking with collect2 signal 7 (Bus error); nextest exited 101 before running tests. The continuation was terminated at the start of maho-rpc. These failures are retained in nextest-maho-cli-resume.log and resume-command-status.log.

At failure, the filesystem had 1.5 GiB free. This run's remaining-maho-cli target contained 8.0 GiB incremental artifacts and 61 GiB dependencies. cargo clean is scoped exclusively to that generated target. The next continuation disables debug information and incremental compilation and uses two Cargo jobs. No test is removed, skipped, or weakened.

Cleanup: bash_1 and mon_BTGCZZBZ8P3K6Q9E were terminated with their process trees through kill_bash. bash_3, bash_4, bash_5, bash_7, bash_8, and bash_9 diagnostic commands completed with exit code 0. st_01a118c4 was cancelled after its accepted audit artifacts had already been integrated.
