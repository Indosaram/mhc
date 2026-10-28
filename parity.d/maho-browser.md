# maho-browser (MIT migration Phase 2, step 4: CUA/browser)

| Source (MIT) | Commit | Rust |
|---|---|---|
| code-yeongyu/omowright `src/pipe.js`, `src/core.js` (CdpClient), `src/dialog-policy.js` | a019fdf | crates/maho-browser/src/cdp.rs, src/transport.rs |
| omowright `src/connection.js` | a019fdf | crates/maho-browser/src/browser.rs |
| omowright `src/core.js` (page, locator, mouse, keyboard) | a019fdf | crates/maho-browser/src/page.rs, src/keys.rs |
| omowright `src/cua.js` | a019fdf | crates/maho-browser/src/computer.rs (`PageComputer`) |
| omowright `src/page-bundle.js` (vendored, renamed globals) | a019fdf | crates/maho-browser/assets/page-snapshot.js |
| code-yeongyu/pi-cua-integration `src/cua/{daemon,protocol,client}.ts`, `src/tools/*.ts` | 9ba0c9e | crates/maho-browser/src/computer.rs (`DesktopDaemon`) |
| pi-cua-integration `python/daemon.py` (vendored) | 9ba0c9e | crates/maho-browser/assets/desktop-daemon.py |
| can1357/oh-my-pi `packages/coding-agent/src/tools/browser/launch.ts` | f59bbac | crates/maho-browser/src/launch.rs |
| oh-my-pi `tools/browser.ts`, `tools/computer.ts` (surface) → spec docs/cleanroom/specs/browser-tools.md | f59bbac | crates/maho-browser/src/extension.rs |

Differences from the originals:

- Tools are discrete actions (`browser`: open/goto/back/forward/reload/snapshot/click/fill/type/
  press/select/hover/scroll/screenshot/evaluate/wait/tabs/close; `computer`: screenshot/click/
  double_click/move/drag/scroll/type/key/sandbox_*/status). oh-my-pi's tools run model-written
  JavaScript (`run`/`call`) over puppeteer; omowright exposes a library; pi-cua-integration uses
  `cua_*` tool names.
- The pipe transport is wired by `/bin/sh` (`3<&0 4>&1`) instead of extra spawn stdio slots, because
  the workspace forbids `unsafe`. Windows (or `$MAHO_BROWSER_TRANSPORT=port`) launches with
  `--remote-debugging-port=0` and reads the endpoint from stderr.
- Events go to a bounded log with sequence numbers instead of an emitter; dialog handling is a
  boolean accept/dismiss policy (omowright also accepts a callback).
- Navigation waits for the lifecycle event only (no network-quiet/DOM-mutation readiness probe,
  `performActionAndWait`, or SPA settle heuristics); actions do not wait for triggered requests.
- Ref resolution uses the page world's `deref` (role+name fallback) without frame-prefixed refs;
  snapshot re-reads return the full tree (no unified diff against the previous snapshot).
- Desktop backend: modifiers during clicks, pointer moves and drags are unsupported (the daemon has
  no such methods); the daemon is written to `$TMPDIR/maho-computer/` and run with
  `$MAHO_COMPUTER_PYTHON`/`python3`.
