# Claude Code plugin hooks — behavior spec (Phase 5)

Scope: finding the hook definitions that Claude Code users already have (their settings files and
their installed Claude Code plugins) and handing them to the hook runtime of `maho-ext-hooks`.
The runtime itself (event names, matchers, command execution, output decisions, trust) is MIT code
ported from senpi and stays as it is; §5 restates its contract only so acceptance tests can run end
to end. What this spec replaces is the installed-plugin discovery that today comes from the SUL
crate `claude-code-compat-core` behind the `claude-plugins` feature of `maho-ext-hooks`. After the
rewrite the feature is MIT-only and on in the MIT profile (`docs/MIT-PROFILE.md` lists it as
compiled out today).

Out of scope: plugin commands, agents, skills, MCP servers and LSP entries (other units), and
installing, updating or removing plugins (Claude Code owns `installed_plugins.json`; mhc only reads).

Suggested neutral names: crate or module `claude_plugins`, entry point "discover plugins", record
"discovered plugin", error record "plugin problem".

## 1. Locations (interface facts, Claude Code conventions)

- Claude directory: `CLAUDE_CONFIG_DIR` when set and non-empty, else `<home>/.claude`
  (`HOME`, then `USERPROFILE`, then the platform home).
- User settings: `<claude dir>/settings.json`.
- Project settings: `<cwd>/.claude/settings.json`, then `<cwd>/.claude/settings.local.json`.
- Plugin database: `<claude dir>/plugins/installed_plugins.json`.
- Per plugin, relative to its install directory: manifest `.claude-plugin/plugin.json`, or
  `plugin.json` at the root when the first does not exist; hooks file `hooks/hooks.json`.

## 2. Settings-file hooks

Each settings file that exists, parses as JSON and has a top-level `hooks` key adds the handlers of
that `hooks` object. The user file has scope `global`; both project files have scope `project`.
Files without `hooks` add nothing and report nothing. A missing file is silent. An unreadable or
malformed file produces one diagnostic naming the path and is skipped; the other files still load.

## 3. Plugin discovery

1. **Database.** Missing file → no plugins, no diagnostic. Malformed JSON → no plugins (logged).
   Three shapes are accepted:
   - `{"version": 1, "plugins": {"<key>": {entry}}}`;
   - `{"version": 2 (or any other value), "plugins": {"<key>": [{entry}, ...]}}`: only the first
     element of each array is used; an empty array or a non-array value skips the key;
   - a top-level array of entries, each with `name`, `marketplace` and `installPath` strings; the
     key is `<name>@<marketplace>`; entries missing any of the three are skipped.
   Entry fields read: `scope` (`user` default, `project`, `local`, `managed`), `installPath`,
   `version`, `projectPath`. Unknown fields are ignored. An entry that does not fit these types is
   skipped.
2. **Enabled.** `<claude dir>/settings.json` → `enabledPlugins` object, looked up by plugin key. A
   boolean value decides; a missing key, a non-boolean value, a non-object `enabledPlugins` or a
   missing/malformed settings file all mean enabled.
3. **Scope.** `user` and `managed` always load. `project` and `local` load only when `projectPath`
   is non-empty and the session cwd is that directory or below it; a leading `~` or `~/`
   (also `~\`) in `projectPath` is expanded to the home directory.
4. **Install path.** If `installPath` exists it is used. Otherwise its parent directory is searched
   for a replacement version: each child directory with a manifest (§1) whose `name` equals the
   name derived from the key (step 6) is a candidate; children with a missing, unreadable or
   differently named manifest are ignored. Preference: a directory name starting with
   `major.minor.patch` beats one that does not; among those, the higher version wins, and for equal
   numbers a plain release beats one with a `-`/`+` suffix; other ties by name order; a directory
   literally named `unknown` comes last. No candidate (or no parent) → a plugin problem carrying
   the key, the configured `installPath` and a reason saying the install directory is missing; the
   plugin is skipped.
5. **Manifest.** Optional. Malformed → treated as absent (logged).
6. **Name.** The manifest `name` when non-empty, else derived from the key: drop a leading `npm:`;
   drop the trailing `@<suffix>` (for a scoped `@org/name@ver` key, the `@` after the slash); a
   scoped `@org/name` is kept whole; otherwise, if what remains contains `/` or `\`, use its last
   path component. Examples: `demo@market` → `demo`; `@acme/kit@2.3.4` → `@acme/kit`;
   `file:///C:/plugins/lib/widget@next` → `widget`.
7. **Version.** The database `version` unless blank or `unknown`; then the manifest `version` if
   non-blank; then the database value if non-blank; else `unknown`.
8. **Result.** Discovered plugins in database order plus the list of plugin problems. Each problem
   becomes a diagnostic naming the plugin key and the reason; discovery of the others continues.

## 4. Plugin hooks

For each discovered plugin whose `hooks/hooks.json` exists:

- Parse it as JSON. Unreadable or malformed → logged, plugin contributes no hooks.
- Replace every `${CLAUDE_PLUGIN_ROOT}` in every string value, at any depth, with the absolute
  install directory (all occurrences).
- Every `command` and `http` handler records the plugin root; other handler types are passed through
  unchanged.
- The handlers get scope `plugin`, timing pre-session, the plugin root, and the manifest path
  `<install>/.claude-plugin/plugin.json`. Placeholder substitution is the guaranteed way a command
  learns its root. The current build does not export plugin variables for these handlers; the
  rewrite should also export `CLAUDE_PLUGIN_ROOT` (= install dir) and `CLAUDE_PLUGIN_DATA`, as
  the runtime already does for its own plugin sources (new behavior, test 19).
- A hooks file whose path already appears among the explicitly configured plugin or runtime hook
  sources is skipped, so the same file never registers twice.

Order of sources, which sets the run order of handlers: mhc's own sources (existing behavior),
then user settings, project settings, project local settings, then plugins in discovery order.
Each later source gets a display order one higher than the highest so far.

## 5. Runtime contract (existing MIT behavior, restated for tests)

- Events: `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `SessionStart`, `PreCompact`,
  `PostCompact`, `Stop`, `Notification`. A file shape is
  `{"hooks": {"<Event>": [{"matcher": "...", "hooks": [{"type": "command", "command": "...",
  "timeout": <seconds>}]}]}}`.
- Matcher (tool events only): empty or `*` matches all; otherwise a `|`/`,` list of names or a
  regular expression, tested against the tool name and its Claude Code spelling (`bash` → `Bash`,
  `edit` → `Edit`, ...). Prompt and stop events ignore the matcher. An invalid regex is a warning
  and falls back to the literal list.
- Command hooks run through the user's shell with the event payload as JSON on stdin, a cleared
  environment plus the allowed variables, and a timeout.
- Decisions: exit code 2 blocks, with stderr as the reason. JSON on stdout may carry `decision`
  (`block`), `reason`, `continue`, and `hookSpecificOutput` (`permissionDecision` `allow`/`deny`/
  `ask`, `updatedInput`, `additionalContext`). A `PreToolUse` deny stops the tool call; `Stop` with
  `decision: block` or `continue: false` keeps the agent going.
- Trust: a command hook runs only after the user trusts it (`/hooks`); plugin hooks are stored in
  the global trust store; changing the command text, matcher or source invalidates trust.

## 6. Platform notes

- Windows: home falls back to `USERPROFILE`; `~\` is expanded; paths in the database may use either
  separator. A handler's `commandWindows`, when present, is used instead of `command` on Windows.
- Path containment for `projectPath` compares whole path components (`/a/proj` does not contain
  `/a/project`).

## 7. Acceptance tests

1. No `<claude dir>` at all → no handlers, no diagnostics.
2. `CLAUDE_CONFIG_DIR` set to a temp dir with `settings.json` containing a `PreToolUse` hook → one
   handler of scope `global`; unset → `<home>/.claude` is read instead.
3. User `settings.json` and project `.claude/settings.local.json` both with hooks → two handlers,
   scopes `global` then `project`; a settings file without `hooks` adds nothing.
4. Malformed project `.claude/settings.json` → one diagnostic naming it; user settings hooks still load.
5. Database v1, v2 and array shapes with the same `user` entry each discover exactly one plugin.
6. v2 entry with two installs, the first for another project and the second for the cwd → plugin
   not loaded (only the first element counts).
7. `project` scope: cwd equal to `projectPath` or a subdirectory loads; a sibling directory, the
   parent, or an empty/missing `projectPath` does not. `~/work/a` matches `<home>/work/a`.
8. `enabledPlugins: {"demo@market": false}` → skipped; `true`, a missing key, or a non-object
   `enabledPlugins` → loaded.
9. Missing `installPath` with sibling directories `2.0.0`, `3.1.0-rc.2`, `3.1.0` (manifests named
   `demo`), `5.0.0` (named `other`) and `4.0.0` (malformed) → `3.1.0` is used and the version
   reported is `3.1.0` when the database says `unknown`.
10. Missing `installPath` and no matching sibling → no plugin, one diagnostic with the key.
11. Manifest in both `.claude-plugin/plugin.json` and `plugin.json` → the first one's name is used.
12. Name derivation: the three examples of §3 step 6, and a key with no manifest name.
13. Plugin `hooks/hooks.json` with `command: "${CLAUDE_PLUGIN_ROOT}/check"` → handler scope `plugin`,
    command `<install>/check`, plugin root recorded, timing pre-session, no diagnostics.
14. Two placeholders in one string are both replaced; placeholders inside nested objects and arrays
    are replaced; `prompt`/`agent` handler entries carry no plugin root.
15. Malformed plugin `hooks/hooks.json` → that plugin adds nothing; other plugins still load.
16. A plugin hooks file also listed as an explicit plugin hook source → registered once.
17. End to end: a trusted plugin `PreToolUse` hook with matcher `Bash` exiting 2 with stderr
    `blocked` → a `bash` tool call is refused with reason `blocked`; a `read` call is unaffected.
18. End to end: the same hook untrusted → it does not run and the tool call proceeds.
19. (New behavior) The hook process sees `CLAUDE_PLUGIN_ROOT` equal to the install directory.
20. MIT profile build (`--no-default-features`) contains no package under `crates/omo/` and still
    passes tests 1–19.
