# Spec: `browser` and `computer` agent tools (MIT profile)

Status: written 2026-10-28 for Phase 2 step 4 (CUA/browser). Sources are MIT only; no SUL source
(`crates/omo/**`, including `maho-omo-browser-bridge` and `maho-omo-computer-use`) was opened.

| Source (license verified MIT) | Commit | Used for |
| --- | --- | --- |
| `code-yeongyu/omowright` | `a019fdfbc337bd917d10b8f3d64026b3549e0b2c` | CDP pipe transport, page/locator/mouse/keyboard model, injected snapshot world, coordinate actions (`cua.js`) |
| `code-yeongyu/pi-cua-integration` | `9ba0c9e23f13a7fc834c28c65e8733b40a162163` | desktop/sandbox daemon protocol, `cua_*` tool set, daemon script |
| `can1357/oh-my-pi` | `f59bbac3d56e8b485fef466388b1cb5be4b93123` | browser tool surface (actions, named tabs, launch options), Chromium discovery and switches, `computer` tool idea |

No single original has the shape mhc needs: oh-my-pi's `browser`/`computer` tools execute
model-written JavaScript in a worker (`run`/`call`) on top of puppeteer, which mhc does not ship;
omowright is a library, not a tool; pi-cua-integration covers only the desktop. This spec fixes the
behaviour of a discrete-action tool surface built from their parts.

## `browser` tool

- One parameter object; `action` is required. Tabs are named, default `main` (oh-my-pi
  `DEFAULT_TAB_NAME`).
- The browser starts on first `open` (or `goto`) and stays open across calls until `close` with
  `all: true` or the end of the session (oh-my-pi releases tabs on settle/idle; mhc closes on
  session shutdown).
- Which browser: `app.cdp_url` or `$MAHO_BROWSER_CDP_URL` attaches to an existing DevTools endpoint
  (`http://host:port` → `/json/version` → `webSocketDebuggerUrl`, or a `ws://` URL); otherwise
  launch `app.path`, else `$MAHO_BROWSER_PATH`/`$PUPPETEER_EXECUTABLE_PATH`/`$CHROME_PATH`, else the
  first existing system Chrome/Chromium/Edge (oh-my-pi `systemChromiumCandidates`). No browser →
  error naming the variables. `app.args` adds switches; `headed` overrides headless (default
  headless, `$MAHO_BROWSER_HEADLESS=0` for headed); `viewport {width,height}`;
  `ignore_https_errors`, `allow_file_access` as in oh-my-pi.
- Launch: owned temporary profile (removed after exit), `--remote-debugging-pipe` on Unix
  (omowright `connectPipe`; no listening port), `--remote-debugging-port=0` elsewhere or with
  `$MAHO_BROWSER_TRANSPORT=port`. Readiness: poll `Browser.getVersion` up to 10 s; failure reports
  the browser's stdio tail. Close: `Browser.close`, 2 s grace, kill.
- `app.target` (with `open`): adopt an existing page whose id, URL or title matches instead of
  creating one (oh-my-pi `app.target`).
- Dialogs: accepted by default; `dialogs: "dismiss"` dismisses (omowright dialog policy).
- Actions and results (every action except `tabs`, `close`, `snapshot`, `screenshot`, `evaluate`
  answers with one line saying what happened, then `Page: <url> — <title>`; `snapshot: true`
  appends a fresh snapshot):
  - `open` (`url?`, `wait_until?`): create or reuse tab `name`, navigate when `url` is given.
  - `goto` (`url`), `back`, `forward`, `reload`: navigate and wait for `wait_until` (`load` default,
    `domcontentloaded`, `networkidle`, `commit`). A navigation error (`errorText`) fails the call;
    not settling within `timeout` (default 30 s) is reported as "(still loading)", not an error.
  - `snapshot` (`interactive?`, `selector?`, `ref?`, `max_depth?`): the omowright accessibility
    tree, YAML-like, interactive elements marked `[ref=eN]`, headed by the URL, title and ref count;
    cut at 60 000 characters with a note on narrowing.
  - `click` (`target` or `x`+`y`, `button?`, `count?` 1–3), `hover` (`target`), `fill` (`target`,
    `text`: replace the value; empty text clears), `type` (`text`, optional `target` focused first;
    key events per character, insertion for characters without a key and newlines), `press`
    (`keys`: `Enter`, `Control+a`, …), `select` (`target`, `values` by value or label), `scroll`
    (`dx`, `dy` default 600, at `target` or the viewport centre, real wheel events).
  - Targets are snapshot refs (`e12`, `[ref=e12]`) or CSS selectors. Before acting the element is
    scrolled into view and must be attached, visible, enabled and stable (omowright `waitForReady`,
    5 s); pointer actions retarget to the real control (`resolvePointerTarget`) and fail when
    another element covers the click point (`checkHitTarget`); `fill` requires an editable element
    (`checkEditable`). A ref that no longer resolves fails with "stale … take a new snapshot".
  - `screenshot` (`full_page?`, `target?`, `path?`): PNG returned as an image block plus
    `Screenshot WxH`; `path` (cwd-relative) also saves it.
  - `evaluate` (`expression`): page JavaScript, promises awaited, the JSON value returned (strings
    raw), cut at 30 000 characters; exceptions fail with their description.
  - `wait`: for `target` visible or `text` in the page (default 10 s), else sleep `timeout`
    (default 1 s).
  - `tabs`: page targets with the names this session gave them. `close`: tab `name`, or
    `all: true` for the browser. An attached (not launched) browser keeps its tabs; they are only
    detached.

## `computer` tool

- `backend`: `browser` (viewport of tab `name`, CSS pixels) or `desktop` (host or Cua sandbox).
  Default: `browser` when tab `name` is open, else `desktop`.
- Actions (both backends unless noted): `screenshot` (image + `Screenshot WxH (<backend>)`),
  `click` (`x`, `y`, `button`, `clicks` 1–3, `modifiers` held during the click — browser only),
  `double_click`, `move`, `drag` (`path`, ≥ 2 points; browser only), `scroll` (`x`, `y`, `dx`,
  `dy`), `type` (`text`), `key` (`keys`: a chord or a list of chords).
  Desktop only: `status`, `sandbox_start` (`mode` local|cloud — cloud needs `CUA_API_KEY` —, `os`,
  `runtime`, `kind`, `version`, `region`, `sandbox` name; the new sandbox becomes the target),
  `sandbox_stop` (`sandbox`, default the current one; the target falls back to the host),
  `sandbox_list`; `sandbox` on other actions selects the target.
- The desktop backend runs the vendored pi-cua-integration daemon with `$MAHO_COMPUTER_PYTHON`
  (default `python3`, `python` on Windows): newline-delimited JSON-RPC on stdio, a `ready` event
  within 20 s, 60 s per call (10 min for `sandbox_start`). Without the `cua` package the daemon
  still starts and actions fail with its "cua Python package is not installed" error; `status`
  says so.

## Not covered

oh-my-pi's code-execution modes, relay/Tern/cmux browsers, stealth patches, Chrome for Testing
downloads, network capture, recording, frames, uploads and downloads; omowright's network snooping,
scroll collection, HAR, tracing, named instances and the extension bridge; pi-cua-integration's
config files and skills.
