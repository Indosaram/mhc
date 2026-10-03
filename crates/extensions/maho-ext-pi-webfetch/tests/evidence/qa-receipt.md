# Registered native webfetch QA receipt

Base: 3462490d. Source pin: 8a4743da6a1f32a6a53f4ddae9484c32c78adcd5.

Environment for every cargo invocation:

```sh
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR=/home/indo/T9-Mac/.cargo-target/maho-code/lane-39
export SENPI_SRC=/home/indo/code/senpi
```

Invocations, in order, final run exit 0:

```sh
timeout 1800 /home/indo/T9-Mac/maho-code-lanes/cargo4 nextest run -p maho-ext-pi-webfetch --no-fail-fast
timeout 1800 /home/indo/T9-Mac/maho-code-lanes/cargo4 clippy -p maho-ext-pi-webfetch --all-targets -- -D warnings
timeout 1800 /home/indo/T9-Mac/maho-code-lanes/cargo4 run -p maho-ext-pi-webfetch --example fetch_qa
bun tools/parity-audit.mjs --crate maho-ext-pi-webfetch
git diff --check
```

`acceptance.txt` contains full actual stdout/stderr: 82 passed, zero skipped;
strict all-target clippy/build clean; registered local execution and 12-row audit PASS.
Factory is `maho_ext_host::loader::load_extensions`, tool invocation is
`maho_ext_host::wrapper::wrap_registered_tool(...).execute`, not helper-only QA.
Fixture session actions preserve the real wrapper's active-tool contract.

## Binary assertions and cleanup

- Markdown/text/HTML: local GET URLs and concrete returned bodies/details captured in acceptance.txt.
- Redirect: original URL `/start` resolves `/final` and returns `redirected`; 21-request loop returns final 302 body in registered test.
- Error: HTTP404 body returns normally with status metadata; invalid scheme, oversized declared/streamed body and network failures are errors.
- Cancellation: pre-aborted custom reason `qa cancellation` preserved; after-header cancellation test waits on exact oneshot then aborts and observes socket EOF.
- Timeout: registered timeout=1 test observes socket EOF, no fixed sleeps.
- Cap: 80,000-byte emoji single line -> 51,200 bytes; 120,000-byte multiline -> 51,191 bytes. Both have outputTruncated=true and correct totals. Small body verbatim.
- All fixture tasks joined. Example rebinds each port after join; no persistent QA server/process or temp fixture file remains.

## Conversion source oracle

`fixtures/pinned-content.json` generated from exact pinned content.ts and pinned
webfetch.test.ts fixture strings. Source TypeScript transpiled using Bun.Transpiler
without semantic changes, then executed with `/usr/bin/node --input-type=module -e`
using installed JSDOM, Readability and Turndown. Includes Tistory body/title priority,
linebreak/table, literal entities and noisy no-wrapper article with relative links.
Native direct conversion plus real registered HTTP invocation match both output formats.
Existing 25-case Turndown fixture matrix and six UTF8 cap tests remain intact.

## Renderer real PTY / browser proof

Invocation: built `debug/examples/render_qa` through `Bun.spawn` with
`terminal:{cols:110,rows:50,data(...)}`; process exit 0. Replay raw ANSI in installed
`@xterm/xterm` 6.0.0, via isolated localhost `Bun.serve`; owned Chromium through
`omowright.connectPipe({browserPath:'/usr/bin/chromium',browserArgs:[...headless...]})`.
Native renderer widths 100 and 45, call plus collapsed/expanded results, Korean body/URL.
`qaPage2.screenshot()` captured 1440x900 PNG, visually inspected: colors, Korean glyphs,
wrapping and blank line separation pass, no overlapping output. Screenshot stored in
`renderer.png.base64` (decode base64 to PNG for inspection).

First screenshot caught fixture HTML charset mojibake, while raw PTY was correct.
Corrected ephemeral HTML response to UTF8; screenshot shown here is corrected.
Cleanup: owned browser.close(), HTTP server.stop(true), task-only temporary browser
profile removed and stat confirmed absent; PTY exit 0. No user's browser touched.

LSP unavailable: rust-analyzer absent in Rust1.95.0 toolchain; daemon also timed out.
Compile/strict-clippy are clean, not falsely reported as LSP success.
