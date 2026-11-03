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
- Sync 5152 additions (method **P**; omowright files byte-identical between a019fdf and
  `c06d20337ad942b09dea8757a45f6c848008a68d`, LICENSE MIT re-checked; oh-my-pi at
  `3a2758b68f7c8e6bdf22c3a76b86c8675e5e0c9e`, root LICENSE MIT):
  - omowright `src/frames-snapshot.js` + the core.js `FrameManager` and multi-frame `takeSnapshot`
    → `src/page/frames.rs`; `src/network-snoop.js` → `src/page/network_snoop.rs`; `src/har.js` →
    `src/page/har.rs`; `src/trace.js` → `src/page/trace.rs`; `src/events.js` (tabs, downloads) →
    `src/page/events.rs`; `src/bridge-transport.js` + `src/bridge-schemas.js` (extension bridge)
    → `src/page/bridge.rs`; core.js `CONSOLE_INTERCEPTOR_SCRIPT`/`PageConsole` →
    `Page::capture_console`/`console_logs` in `src/page.rs`.
  - oh-my-pi `tools/browser/launch.ts` stealth section → `src/launch/stealth.rs`, its
    `tools/puppeteer/*_stealth_*.txt` → `assets/stealth/` (vendored unchanged) and the injection
    wrapper → `assets/stealth/injection.js`; its `ensureChromiumExecutable`/`ensureManagedChromium`
    plus the `@puppeteer/browsers` cache layout and download URL scheme (Apache-2.0, facts only:
    paths and URLs) → `src/launch/chrome_for_testing.rs`.
- Not ported: oh-my-pi's JavaScript-execution browser/computer runtimes (puppeteer workers,
  native desktop capture), relay/Tern/cmux browsers; omowright's scroll collection, routes, named
  instances, cookies, Chrome profile APIs, captcha helpers, the standalone `src/chrome.js` API and
  the bridge's companion extension (`bridge/extension/background.js`); pi-cua-integration's config
  loader, sandbox manager policy and skills.
- No SUL source was opened: not `crates/omo/**` (including `maho-omo-browser-bridge` and
  `maho-omo-computer-use`), `assets/omo-latest/**`, `assets/omo.schema.json`, the omo-gated
  `maho-cli` modules, `.omo/`, nor any OmO/COMP source.
- Tests were written for this crate (a scripted stand-in browser speaking CDP over the pipe, the
  real vendored daemon, and a real Chromium when `MAHO_BROWSER_TEST_PATH` is set); no upstream or
  OmO fixtures were reused.
