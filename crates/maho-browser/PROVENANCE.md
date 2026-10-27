# Provenance — maho-browser

- License: MIT. Third-party notices: `LICENSE-THIRD-PARTY.md`.
- Method: **P** (port of MIT originals) for the CDP client, transports, page operations, keyboard,
  Chromium discovery, both computer-use backends and the vendored page world and desktop daemon;
  **C** (from `docs/cleanroom/specs/browser-tools.md`, itself written from the MIT sources below)
  for the discrete-action `browser`/`computer` tool surface, which has no direct original.
- Origins (each LICENSE checked as MIT before reading; checkouts in `/workspace/work/upstream`):
  - `code-yeongyu/omowright` @ `a019fdfbc337bd917d10b8f3d64026b3549e0b2c` (LICENSE MIT, Copyright
    2026 Yeongyu Kim; `package.json` `"license": "MIT"`):
    - `src/pipe.js` (`PipeCdpClient`: spawn with `--remote-debugging-pipe`, NUL framing, pending map,
      command timeout, readiness poll, stdio tail, graceful close), `src/core.js` WebSocket
      `CdpClient` (wire handling, `/json/version` discovery), `src/dialog-policy.js` → `src/cdp.rs`,
      `src/transport.rs`;
    - `src/connection.js` (`BrowserConnection`: targets, `newTab`, `attachPage`, `closeTab`,
      `close`) → `src/browser.rs`;
    - `src/core.js` page/locator/mouse/keyboard (`Page.navigate` + lifecycle waits, evaluation by
      value, ref locators through the page world, `mouseMoved`/`mousePressed`/`mouseReleased`,
      `mouse.wheel`, `screenshot`, `KEYBOARD_LAYOUT`, `splitKeyCombo`, `resolveKeyDescriptor`,
      modifier mask) → `src/page.rs`, `src/keys.rs`;
    - `src/cua.js` (`createCua`) → `PageComputer` in `src/computer.rs`;
    - `src/page-bundle.js` → `assets/page-snapshot.js` (vendored; `__omowright` →
      `__mahoPage`, ref symbol and setup function renamed; nothing else changed).
  - `code-yeongyu/pi-cua-integration` @ `9ba0c9e23f13a7fc834c28c65e8733b40a162163` (LICENSE MIT,
    Copyright 2026 Yeongyu Kim; NOTICE kept in `LICENSE-THIRD-PARTY.md`):
    - `src/cua/daemon.ts`, `src/cua/protocol.ts`, `src/cua/client.ts`, `src/tools/*.ts` →
      `DesktopDaemon`, `DesktopTarget`, `SandboxRequest` in `src/computer.rs` and the desktop
      actions of the `computer` tool;
    - `python/daemon.py` → `assets/desktop-daemon.py` (vendored unchanged apart from a header).
  - `can1357/oh-my-pi` @ `f59bbac3d56e8b485fef466388b1cb5be4b93123` (root LICENSE MIT):
    - `packages/coding-agent/src/tools/browser/launch.ts` (`systemChromiumCandidates`,
      `resolveSystemChromium`, `buildHeadlessLaunchArgs`, `PUPPETEER_*` variables) →
      `src/launch.rs`;
    - `packages/coding-agent/src/tools/browser.ts`, `tools/computer.ts` (tool surface: actions,
      named tabs, `app.*` options, viewport, dialogs, close) → the spec and `src/extension.rs`.
- Not ported: oh-my-pi's JavaScript-execution browser/computer runtimes (puppeteer workers,
  native desktop capture), relay/Tern/cmux browsers, stealth patches, Chrome for Testing download;
  omowright's frames, network snooping, scroll collection, routes, HAR, tracing, named instances,
  cookies, Chrome profile APIs, captcha helpers and extension bridge; pi-cua-integration's config
  loader, sandbox manager policy and skills.
- No SUL source was opened: not `crates/omo/**` (including `maho-omo-browser-bridge` and
  `maho-omo-computer-use`), `assets/omo-latest/**`, `assets/omo.schema.json`, the omo-gated
  `maho-cli` modules, `.omo/`, nor any OmO/COMP source.
- Tests were written for this crate (a scripted stand-in browser speaking CDP over the pipe, the
  real vendored daemon, and a real Chromium when `MAHO_BROWSER_TEST_PATH` is set); no upstream or
  OmO fixtures were reused.
