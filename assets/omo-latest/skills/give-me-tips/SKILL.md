---
name: give-me-tips
description: Explains mhc tips in depth. Use when the user quotes or asks about a "Tip:" line from the mhc terminal (at startup or while the agent is working), names a tip id, asks how a feature a tip advertised really works, asks which tips exist or which ones they personally can see, wonders why a tip keeps or never appears, or wants tips switched off. Pulls the live tip list with `mhc --list-tips`, reads the user's settings.json to work out which tips are visible to them, then reads the mhc source behind the feature so the answer describes real triggers, mechanics, settings and limits instead of repeating the one-line hint.
---

# give-me-tips

A tip is one line of text. The user has already read it. Your job is to explain the machinery behind it: what triggers the feature, what mhc actually does step by step, what can be configured, and where it stops. Every answer is built at run time from three sources, in this order: the live tip list, the user's settings, and the mhc source code. Do not answer from memory.

## Ground rules

- **Language.** Answer in the language the user wrote in. Keep tip ids, file paths, flags, slash commands and setting keys exactly as they are.
- **Proportion.** Open with a two-to-three-sentence answer to what they asked, then give the detail.
- **Tone.** Be precise and openly interested in how the thing is built. Prefer concrete mechanics (events, files, keys, defaults, limits) over adjectives. No sales language.
- **No canned essays.** Nothing in this skill describes a particular tip's feature. Derive each explanation fresh from the list and the code.
- **Code beats the hint.** If the tip text and the implementation disagree, say so plainly and describe what the code does.

## Step 1: get the live tip list

Run:

```bash
mhc --list-tips
```

If `mhc` is not on PATH, use the command the user launched this session with (a branded build of mhc may install under a different command name; it accepts the same flag).

The output is a JSON array, one object per catalog entry:

```json
{ "id": "<tip id>", "text": "<rendered tip>", "requiresCommand": "<command>" }
```

- `text` has key placeholders filled with the **default** keybindings (several keys joined with `/`). The user's own keys may differ.
- `requiresCommand` appears only on tips tied to a slash command or feature that may not exist in every session.
- This array is the whole catalog. It is not what any one user sees (see Step 3).

### Matching the user's tip

1. If they gave an id, look it up exactly.
2. Otherwise take a distinctive fragment of what they quoted, collapse whitespace on both sides, and search the `text` fields. Ignore key names in the quote when matching, since their bindings may not be the defaults. A leading `Tip: ` is part of the display, not the text.
3. Several candidates: pick the best fit, and mention the others by id.
4. No candidate: it may be a component notice rather than a catalog tip (see "Notices that look like tips"). Check that before telling the user the tip is unknown.

### If the flag does not work

If the command is missing, errors out, or rejects `--list-tips`, read the catalog from source and tell the user you did so:

- Local checkout of mhc if one is present (look in the working directory and its parents, or ask where it is); otherwise the public repository `https://github.com/Indosaram/mhc` at the same paths.
- `crates/maho-interactive/src/tips/registry.rs` — `TIP_DEFINITIONS`, the groups concatenated in display order.
- `crates/maho-interactive/src/tips/catalog/*.rs` — one file per group (model, input, session, workspace, settings, cli, subagent, memory, dag, computer, ethos, ...).
- `crates/maho-interactive/src/tips/catalog/types.rs` — each `TipDefinition` has an `id`, a `bindings` list, an optional `requires_command`, and a `render` function that builds the text from key names.

## Step 2: read the code behind the feature

Before describing behavior, open the implementation of whatever the tip advertises: the slash command handler, the setting it reads, the tool, or the component. Find it by searching the mhc source for the command name, the setting key, a phrase from the tip, or the id. For orientation only: a memory-related tip will lead somewhere under `crates/omo/components/maho-omo-memory/`; interactive UI behaviour usually lives in `crates/maho-interactive/`; CLI flags in `crates/maho-cli/src/cli/`.

Then explain, from what you read:

- **Trigger** — the exact command, key, setting, or flag.
- **Sequence** — what mhc does when it fires: events, messages, files or state it writes, settings it consults, defaults and limits it applies.
- **Control** — how to enable, disable, or tune it.
- **Edges** — notable failure modes, interactions, or conditions under which it does nothing.

List the file paths you actually opened. If no source is reachable at all, say the explanation is unverified and keep to what the tip text itself states; do not invent internals.

## Step 3: work out what this user can see

Do this whenever the user asks what they can see, why a tip does or does not appear, or how to turn tips off; for other questions, add a short visibility note if it matters.

**Find the settings file.** It is `settings.json` inside the agent directory:

- `$MAHO_CODING_AGENT_DIR`, if set and non-empty (a branded build uses its own `<PREFIX>_CODING_AGENT_DIR`);
- otherwise `~/.omo/agent/` for standalone mhc;
- a branded build with a flat layout may keep it directly in its config folder in the home directory.

If the file does not exist, defaults apply: tips on, startup tip shown, no history.

**Keys that matter:**

| Key | Default | Effect |
|---|---|---|
| `tips` | `true` | `false` turns off both startup and working tips. |
| `quietStartup` | `false` | `true` hides the startup tip; working tips still show. |
| `tipsHistory` | empty | Map of tip id → last time shown (milliseconds since epoch). Updated every time a tip is displayed. |

User key overrides live in `keybindings.json` in the same agent directory.

**How the next tip is chosen** (`crates/maho-interactive/src/tips/scheduler.rs`, `select_tip`). mhc walks the catalog in registry order and:

1. skips ids in the exclude set (for working tips, the ones already shown this session — `working_tip.rs`);
2. skips a tip that lists bindings when none of those bindings maps to any key for this user;
3. skips a tip whose `requiresCommand` is not available in the session;
4. returns the first remaining tip that has never been shown (absent from `tipsHistory`); if every remaining tip has been shown, returns the one shown longest ago.

So a user works through unseen tips first, then cycles by least recently seen, and a tip tied to an unavailable command or to keys they have unbound never shows up. Check the call sites (startup in `startup_tip.rs` and its caller, working tips in `working_tip.rs`) to see which of these gates are actually wired up in the build you are reading, and report what the code does.

**What to report:**

- whether tips are on at all, and whether the startup tip is hidden;
- which tips are excluded and why (command not available, keys unbound);
- which have been shown and roughly when (turn the millisecond timestamps into dates or "n days ago");
- which unseen tip is likely next, by the rule above.

Never present the whole catalog as though this user sees all of it.

## Listing mode

When asked for "all tips" or "the tips I can see":

- group them (by catalog group, inferred from the id prefix or the source file, or by gate);
- give each one its id and a one-line gist in your own words;
- mark any that are hidden for this user and the reason (gated command, unbound keys, tips off, startup hidden);
- offer to go deep on any one of them.

## Turning tips off or on

Explain `tips` and `quietStartup` and where the settings file lives. Edit the user's `settings.json` only when they explicitly ask you to; when you do, change just that key, keep the rest of the JSON intact, and show what changed.

## Notices that look like tips

Some messages in the UI read like tips but come from components, not from the catalog, so they will not be in `--list-tips`. Examples are the notice shown when mhc falls back to another model, and the notice shown when stored memory is recalled into the conversation. They are rendered as custom message types such as `omo-fallback-architect:tip`, `omo-fallback-architect:notice`, or `omo-kibitzer:nudged`.

Handle every such case the same way: search the mhc source (`crates/omo/components/`, `crates/maho-cli/src/cli/`) for the exact text the user saw or for the custom type, find the component that emits it, and explain from that code — the condition that triggers it, what the component does, and the switch that controls it (for instance a settings key such as `memory.recall.enabled`, if that is what the code reads). Tell the user this is a component notice, not a catalog tip.

## Startup tip format, for recognising quotes

A startup tip is printed as `Tip: <text>` with newlines in the text collapsed to spaces, followed by a line pointing to this skill. Working tips use the same catalog, appear while the agent is busy, and do not repeat within a session.
