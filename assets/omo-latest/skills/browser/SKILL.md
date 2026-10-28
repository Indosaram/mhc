---
name: browser
description: "Drives a real browser through the omowright library from the js eval kernel: sites the user is already signed into, forms and clicks, JS-rendered pages, screenshots, web QA, extension popups, a human handoff for login, CAPTCHA or OTP, and a browser you own for scraping, bot-scored targets, network capture and QA traces. Use for any interactive browser task; not for a plain search or an unblocked static fetch."
---

# Browser

One library, two engines. omowright ships inside this skill; choose the engine before you act:

| You need | Engine | Entry point |
|---|---|---|
| A site the user is signed into, their open tabs, a form, a click-through, a screenshot, web QA, an extension popup | **attached** — the user's own browser through BrowserSkill | `connectBrowserSkill()` |
| A throwaway profile, bot-scoring evasion, a CAPTCHA, network interception, a QA flight trace, coordinate control, headless runs | **owned** — a browser your code launches | `connectPipe()` / `connectCloakProfile()` — [references/owned-engine/README.md](references/owned-engine/README.md) |
| Plain text or data extraction from a URL, getting past a 403, a platform that blocks fetchers | neither | the `ultimate-browsing` skill |

**Attached is the default,** because it is the only engine carrying the user's logins and the only
one where a human is a single call away. Never substitute one engine for the other silently: if
the attached engine is not set up, run the onboarding script and tell the user its one remaining
step.

## Step 0 — which engine this session may use

A host can pin the engine through the `OMO_BROWSER_ENGINE` environment variable. **When it is set, it
overrides the table above**; when it is unset or empty, the table applies as written and the library is
the plain one.

| `OMO_BROWSER_ENGINE` | What you may do |
|---|---|
| `connected` | Only the user's own browser, through `connectBrowserSkill()`. The owned engine is off. |
| `builtin` | Not this library: the host gives you its own in-app browser tools. Use those. |
| `none` | No agent browser at all for this project. Tell the user; do not reach for any other browser. |
| unset / empty | Everything in this skill, chosen with the table above. |

While the variable is set, `loadOmowright()` hands you a **guarded** library:

- The owned engine (`connect`, `connectPipe`, `connectCloakProfile`) and every other export that could act
  on a browser are replaced by stubs that refuse. Do not hunt for a way around them; tell the user what this
  session allows. Read-only and diagnostic helpers (`bskDoctor`, `toHar`, …) work as usual.
- Under `connected`, the host can show what the browser is doing, and you are asked to confirm before
  anything irreversible: sending, posting, paying, ordering, subscribing, deleting, closing an account, and
  pressing Enter in a message box. The guard asks the user through the host's question tool and waits.
- A "no" arrives as `BrowserActionDeclinedError`. Report it. Never retry the action or route around it —
  `session.tool()` only accepts read-only raw tools, and `evaluate` scripts that click, submit or send are
  checked too.
- The user's Stop arrives as `BrowserUserStoppedError`. Tell the user and start no new browser session in
  this turn.
- The guard exists to keep a cooperating agent from making mistakes. **It is not a security boundary**:
  importing the raw entry from `resolveOmowrightEntry()` bypasses it, and without the host's bridge tool the
  host can neither show browser state nor deliver Stop (confirmation questions are still asked).

Error types and codes are listed in [references/commands.md](references/commands.md#errors).

## Step 1 — load omowright and prove the stack

```js
const { loadOmowright } = await import("<skill-root>/scripts/omowright.mjs")
const { omowright } = await loadOmowright()          // { connectBrowserSkill, bskSnapshot, connectPipe, ... }
```

```bash
node "<skill-root>/scripts/browser-doctor.mjs" --json
```

| State | Meaning | Next |
|---|---|---|
| `ready` | CLI, daemon and a connected browser | start a session |
| `no-cli` / `no-daemon` / `no-extension` | something is missing | `node "<skill-root>/scripts/browser-install.mjs" [--browser=<id>]` prepares everything it can for **the browser the user uses**, then prints the **single** step only the user can do (relaunch that browser and click **Enable**); relay it verbatim, wait, re-run the doctor |
| `choose-browser` | the signals do not single out one browser (Safari/Firefox default, an idle default while another browser runs, several in use) | nothing was installed; take the browser from memory or ask the user, then `browser-install.mjs --browser=<id>` |
| `no-browser-support` | no Chromium-family profile on this machine | say so and stop |

**Install into the browser the user actually uses, never into whatever happens to be on disk.** Before
installing, check your memory for the user's browser; otherwise read the doctor's `browser` (picked
from the OS default browser, running apps and recent use — `candidates` shows the evidence). If memory
and the doctor disagree, or the doctor says `choose-browser`, ask the user. Pass the answer as
`--browser=<id>` and record it in memory. A Chrome that is merely installed is not their browser.

**Never launch a headless browser because the attached one is missing.** It has none of the
user's sessions, so every login turns into a ladder you should not be climbing. Say which state
you hit and ask.

## The loop (attached)

```js
const session = await omowright.connectBrowserSkill({ name: "<task>", focused: false })
try {
  await session.navigate("https://example.com/", { waitUntil: "load" })
  const { tree, refs, css } = await omowright.bskSnapshot(session, { interactive: true })  // OmOWright tree + refs, no trace in the page
  await session.click({ selector: css.e3 })                                                 // css[ref] is null inside shadow roots:
  const vom = await session.observe({ maxTokens: 4000 })                                    //   then read the daemon's own tree ...
  await session.click("@e7")                                                                //   ... and click its @eN ref
  await session.fill(css.e5, "hello")
  await session.press("Enter")
  await session.waitForNavigation({ waitUntil: "load" })
  const shot = await session.screenshot()                                                   // { buffer, width, height, captureId }
} finally {
  await session.stop()                                                                      // success AND failure; returns borrowed tabs
}
```

1. **Read before every action.** `bskSnapshot` refs and `observe` `@eN` refs are reissued on each
   call; use a ref in the same cycle you read it.
2. **Navigation and large DOM changes stale every ref.** Read again rather than reusing.
3. **Two identical failures mean change approach, not retry.** A third identical attempt is a defect.
4. **Borrow a user tab explicitly** (`tabList({ scope: "user" })`, `tabBorrow(id)`, `tabReturn(id)`).
   Borrowing prompts the user; never invent tab ids and never repeat a denied borrow.
5. **Always `stop()` the session,** on success and on failure.

Every method, its options, and the failure codes are in [references/commands.md](references/commands.md).

## When a human is the only way through

Login, CAPTCHA, OTP, a payment confirmation, a consent dialog:

```js
const outcome = await session.requestHelp({ prompt: "<what you need done>", targets: ["@e4"], timeoutMs: 300_000 })
```

Then read the page again. Respect a `cancelled` or `timed_out` outcome; do not work around it by
changing the extension's automation settings.

## Rules

- **Never read credentials through the page.** No `evaluate` that extracts a password, token,
  cookie or recovery code. The value of the attached engine is that the browser is already signed in.
- **Never clear cookies, cache or site data.** It is the user's real profile; clearing it logs them
  out everywhere. No flow here needs it.
- **`focused: false` by default.** The browser belongs to someone who is probably using it.
- **One short, named session per task,** always stopped.
- **Bot-scored or WAF targets go to the owned engine.** The attached engine's daemon enables console
  capture on every tab it drives, which is a known automation signal; CloakBrowser through
  `connectCloakProfile()` is the stealth path. **Exception:** while `OMO_BROWSER_ENGINE` is set, do not
  switch engines — tell the user the site needs a browser this session does not allow.

## Where the rest lives

| Topic | Read |
|---|---|
| Session methods, targets, options, error codes | [references/commands.md](references/commands.md) |
| Installing: CLI, daemon, extension, the one human step, blocklisted extension | [references/install.md](references/install.md) |
| Agent on one machine, browser on another | [references/remote.md](references/remote.md) |
| Owned engine: launch, snapshot ladder, network, frames, human handoff | [references/owned-engine/README.md](references/owned-engine/README.md) |
| Reading a 1Password vault the user has unlocked | [references/recipes/1password.md](references/recipes/1password.md) |
