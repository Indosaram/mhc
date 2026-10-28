# Provenance — maho-comment-checker

- Method: **P** (port of MIT originals).
- Origins (MIT, licenses verified before reading):
  - `code-yeongyu/pi-comment-checker` @ `0a38dd8ff362be1b6020f2baba7b5723cbc5ea76`
    (`src/core.ts`: hook input/tool input shape, `getString`, apply_patch metadata readers;
    `src/cli.ts`: package binary lookup).
  - `code-yeongyu/go-claude-code-comment-checker` @ `311565fc9380579449ae34a8ab4c2cd386da840c`
    (`npm/comment-checker/index.js`: `getBinaryPath` order — platform package, `vendor/<platform-arch>`,
    `bin/`; `src/api.rs`: hook input fields the checker reads and its exit codes 0/2).
- Additions (neutral, not in the originals): `COMMENT_CHECKER_BINARY` override and a `PATH` lookup as
  final fallbacks, so a natively installed checker works without a node_modules tree.
- The API shape (`HookInput`, `HookToolInput`, `EditPair`, `get_string`,
  `get_apply_patch_metadata_files`) follows the MIT call sites in
  `crates/extensions/maho-ext-pi-comment-checker`. No SUL source (`crates/omo/**`) was opened.
- License: MIT.
