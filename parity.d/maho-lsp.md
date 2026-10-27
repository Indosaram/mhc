# maho-lsp / maho-comment-checker (MIT migration Phase 2, step 1)

| Source (MIT) | Commit | Rust |
|---|---|---|
| code-yeongyu/pi-lsp-client `src/lsp/transport.ts`, `connection.ts`, `client.ts` | fc8f612 | crates/maho-lsp/src/{rpc,client}.rs |
| pi-lsp-client `manager.ts`, `client-wrapper.ts` | fc8f612 | crates/maho-lsp/src/{manager,tools}.rs |
| pi-lsp-client `config-loader.ts`, `server-resolution.ts`, `server-installation.ts` | fc8f612 | crates/maho-lsp/src/config.rs |
| pi-lsp-client `server-definitions.ts`, `language-mappings.ts` | fc8f612 | crates/maho-lsp/src/servers.rs (generated) |
| pi-lsp-client `tools/*.ts`, `directory-diagnostics.ts`, `infer-extension.ts` | fc8f612 | crates/maho-lsp/src/tools.rs |
| pi-lsp-client `formatters.ts`, `workspace-edit.ts` | fc8f612 | crates/maho-lsp/src/{format,edit}.rs |
| pi-lsp-client `post-edit-diagnostics.ts`, `index.ts` | fc8f612 | crates/maho-lsp/src/{post_edit,extension}.rs |
| pi-lsp-client `errors.ts`, `utils.ts` | fc8f612 | crates/maho-lsp/src/error.rs |
| can1357/oh-my-pi `packages/coding-agent/src/lsp/mux/{protocol,daemon}.ts` (design) | f59bbac | crates/maho-lsp/src/daemon.rs, src/bin/mhc-lsp.rs |
| code-yeongyu/pi-comment-checker `src/core.ts`, `src/cli.ts` | 0a38dd8 | crates/maho-comment-checker/src/{hook,binary}.rs |
| code-yeongyu/go-claude-code-comment-checker `npm/comment-checker/index.js`, `src/api.rs` | 311565f | crates/maho-comment-checker/src/binary.rs, hook.rs |

Not ported: pi-lsp-client `inspector.ts`, `renderers.ts` (TUI). Hook interfaces
(`crates/maho-lsp/src/hooks.rs`) are new.
