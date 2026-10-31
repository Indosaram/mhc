# LSP client: built-in server initialization options (`maho-lsp`)

Status: delta-only spec, created 2026-11-01 for the omo v5.1.52 sync. `crates/maho-lsp` is a port
of MIT originals (see its `PROVENANCE.md`, method P); it had no clean-room spec before. This
file states only the behaviour the v5.1.52 rows add, in terms of what `maho-lsp` already has:
the built-in server table (`src/servers.rs`), the merged server list built from the
`lsp-client.json` config files (`src/config.rs`, sources project, user and builtin), and the
`initializationOptions` sent in the LSP `initialize` request (`src/client.rs`).

Interface facts used below: the config key `initialization` of an `lsp-client.json` server entry
(already read by `maho-lsp`); the LSP `initialize` request field `initializationOptions`; the
`typescript-language-server` initialization options `tsserver.useSyntaxServer` (value `"never"`)
and `disableAutomaticTypingAcquisition` (value `true`), which are documented options of that
public server.

## Delta (omo v5.1.52, 2026-11-01)

### D1. Built-in servers may carry default initialization options

Behaviour: an entry of the built-in server table may carry a default initialization-options
object. In this release exactly one does: the `typescript` server (command
`typescript-language-server --stdio`) defaults to

- `tsserver.useSyntaxServer` = `"never"`, and
- `disableAutomaticTypingAcquisition` = `true`.

Purpose (user-visible): every TypeScript session starts one tsserver process instead of a
semantic plus a syntax-only tsserver, and no typings installer process is started. The LSP tools
only use semantic requests, so no answer changes.

When the built-in entry is used as is (no config entry for its id), the merged server carries
these options and the client sends them as `initializationOptions` in `initialize`. Every other
built-in server still sends no `initializationOptions`.

### D2. Merging built-in and configured initialization options

When a config file has an entry for an id that also exists in the built-in table, the merged
server's initialization options are:

| configured entry | options of the merged server |
| --- | --- |
| no `initialization`, and no own `command` | the built-in defaults |
| `initialization` given, and no own `command` | the built-in defaults with the entry's keys added; on a key present in both, the entry's value wins |
| has its own `command` (with or without `initialization`) | only the entry's `initialization` (none if absent); the built-in defaults are not applied |

The merge is shallow: only top-level keys are combined. A top-level key the entry sets replaces
the built-in value for that key as a whole, even when both values are objects. A config entry
for an id that has no built-in entry is unchanged: its options are its own `initialization`.
The precedence of sources (project over user over builtin) and the `disabled` rule are
unchanged; this delta only decides which options the winning server carries.

Note for `maho-lsp`: today a configured entry is used only when it has both `command` and
`extensions`. An entry without a `command` therefore never wins over the built-in. To make the
first two rows reachable, an entry for a built-in id that leaves `command` out (and/or
`extensions`) takes the missing field from the built-in entry. An entry for an id without a
built-in entry still needs both fields and is skipped otherwise, as today.

Errors: none. A non-object `initialization` value in a config entry is used as is when the
entry has its own `command` (current behaviour). When it would be merged with built-in defaults,
it replaces them as a whole, because there are no keys to merge.

### D3. Tests (given / when / then)

1. Given no config files, when the merged list is built, then the `typescript` server carries
   `{"tsserver": {"useSyntaxServer": "never"}, "disableAutomaticTypingAcquisition": true}` and
   the `deno` server carries no initialization options.
2. Given a user entry `{"typescript": {"initialization": {"preferences": {"quotePreference":
   "single"}}}}` (no command), then the `typescript` server keeps the built-in command and
   extensions, and its options have all three top-level keys: `tsserver`,
   `disableAutomaticTypingAcquisition` and `preferences`.
3. Given a user entry `{"typescript": {"initialization": {"disableAutomaticTypingAcquisition":
   false}}}`, then that key is `false` and `tsserver.useSyntaxServer` is still `"never"`.
4. Given a user entry `{"typescript": {"initialization": {"tsserver": {"logVerbosity":
   "off"}}}}`, then `tsserver` is exactly `{"logVerbosity": "off"}` (shallow merge).
5. Given a user entry `{"typescript": {"command": ["my-ts-ls", "--stdio"], "extensions":
   [".ts"]}}`, then the server has no initialization options. With an added
   `"initialization": {"a": 1}`, its options are exactly `{"a": 1}`.
6. Given a project entry `{"typescript": {"extensions": [".ts"]}}`, then the server has
   source project, the built-in command, extensions `[".ts"]` and the built-in options.
7. Given an entry for an unknown id `{"foo": {"initialization": {"a": 1}}}` without `command`,
   then no server `foo` is produced.
8. Given a fake server started for the merged `typescript` entry of test 1, when the client
   sends `initialize`, then the request's `initializationOptions` equals the object of test 1.

### D4. Mapping

| census row | delta § | APPLIES/N-A | reason |
| --- | --- | --- | --- |
| omo-cores-15 | D1 | APPLIES | the built-in TypeScript server starts with the two options, so fewer processes run |
| omo-cores-16 | D2 | APPLIES | which options win when a config entry exists for a built-in id |

Leak self-check:

- [x] no fenced code block; the JSON in the tests is user config and LSP wire content;
- [x] no sentence translates or paraphrases a SUL comment, doc comment or prompt; the purpose
      line is written from the public options' documented effect;
- [x] the only constants are the two public `typescript-language-server` option names and
      values (interface facts of that server) and existing `lsp-client.json` keys;
- [x] no new identifier is proposed beyond plain words; no OmO-original name;
- [x] no OmO-original feature is specified.
