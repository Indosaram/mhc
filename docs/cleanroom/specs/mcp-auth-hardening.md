# MCP client auth and transport hardening — behavior spec (unit `mcp-auth-hardening`)

Scope: hardening features of the MIT MCP client `crates/builtins/maho-ext-mcp` (a port of senpi's
MCP client): OAuth scope step-up, post-rejection token refresh, OAuth server discovery with a
process cache, the HTTPS rule for discovered endpoints, the RFC 8707 resource parameter, secret
redaction in transport errors, secret filtering of the environment handed to stdio servers, a
retrying log file sink and process-wide refresh coalescing.

Sources this spec is written from (all public or MIT): RFC 6750 §3 (Bearer `WWW-Authenticate`,
`insufficient_scope`), RFC 6749 §3.3 (scope syntax), RFC 8707 (resource indicators), RFC 9728
(protected resource metadata), RFC 8414 (authorization server metadata), the MCP specification
"Authorization" section (2025-06-18 and later: protected resource metadata discovery, resource
parameter, scope challenge handling), and the MIT `@modelcontextprotocol/sdk` TypeScript client
(`client/auth.ts`, `client/streamableHttp.ts`, `client/stdio.ts`). The implementer may read all of
these.

Everything below is observable behavior. Names are proposals; the implementer may choose other
neutral names.

## 0. Starting point

The crate today (after the strip that precedes this unit) has: a stdio and a streamable-HTTP
client (`transport_sdk.rs`), an OAuth provider/token store with a persisted `discoveryState`
record field, a per-manager refresh single-flight (`oauth_refresh.rs`), loopback and paste
authorization flows (`commands_auth.rs`), and a log ring buffer with an optional file sink
(`log.rs`). An HTTP non-2xx response currently fails the request immediately.

## 1. Resource indicator (RFC 8707)

1. The resource value for a server is its configured URL normalized as: parse as a URL; drop the
   query and the fragment; serialize; if the result ends in exactly one `/` after the
   path, remove that one trailing `/`. Examples: `https://a.test` → `https://a.test`;
   `https://a.test/mcp/?t=1#f` → `https://a.test/mcp`; `https://a.test/mcp//` → `https://a.test/mcp/`.
   An unparsable URL is used unchanged.
2. The `resource` parameter is sent on: the authorization request URL, the authorization-code
   token request, the refresh-token request (a resource stored with the tokens wins over the
   computed one) and the client-credentials token request.
3. Adding the parameter to a form that already carries `resource` replaces the first occurrence
   in place, removes later duplicates, and keeps every other field in order; otherwise it is
   appended.

## 2. Discovery (RFC 9728 → RFC 8414) with a process cache

1. Protected resource metadata is fetched from `/.well-known/oauth-protected-resource` resolved
   against the server URL's **origin** (path, query and fragment of the server URL are dropped).
   - 2xx with a JSON object: its `authorization_servers` array must contain at least one
     non-empty string; the first is the issuer. None → error whose message says the protected
     resource metadata lacks `authorization_servers`.
   - 404: fall back to treating the server URL itself as the issuer, with no resource metadata.
   - Any other status: error that includes the numeric status.
   - A 2xx body that is not JSON (or is JSON `null`) → error saying the metadata is not valid JSON.
2. Authorization server metadata is fetched from `/.well-known/oauth-authorization-server` +
   the issuer's path with trailing `/` removed (RFC 8414 §3.1), resolved against the issuer's
   origin. If that returns 404 and the issuer path was non-empty, retry once at the origin form
   (`/.well-known/oauth-authorization-server`). 404 with nothing left to try → "not found" error;
   other statuses → error with the status.
3. The metadata must have non-empty `authorization_endpoint` and `token_endpoint` strings; a
   missing one → error naming the missing field. `registration_endpoint` is optional. Each
   present endpoint is parsed and stored in normalized URL form.
4. The result (issuer URL, authorization server metadata, resource metadata or null) is cached
   **in process memory only**, keyed by the normalized server URL. A cache hit makes no network
   request. Concurrent lookups for the same key while a fetch is in flight share that one fetch
   (exactly one request per endpoint). A failed fetch is not cached and does not poison later
   lookups; when the in-flight entry settles it is removed only if it is still the same entry
   (a newer one for the same key is left alone).
5. A public function clears the cache and the in-flight table. Invalidating the provider's
   "discovery" credential scope calls it.
6. Discovery state is no longer persisted: the `discoveryState` field is removed from the token
   record; a legacy record that still has `discoveryState` loads fine, the key is dropped on the
   next read and is not written back. All other fields, including unknown keys, survive.
7. An injected discovery function (the existing `RefreshManagerOptions.discover`) always wins
   over the process cache.

## 3. HTTPS rule

1. The provider has a boolean "require HTTPS", default **true**. When true, the server URL, the
   issuer URL and every endpoint taken from authorization server metadata must use the `https`
   scheme; otherwise discovery fails with a message of the form `<label> must use https`, where
   label is one of `Resource server URL`, `Authorization server URL`, `authorization_endpoint`,
   `token_endpoint`, `registration_endpoint`.
2. Tests that use loopback HTTP fixtures set it to false explicitly. Production code paths never
   set it to false.

## 4. Scope step-up (RFC 6750 `insufficient_scope`, MCP scope challenge)

1. Challenge parsing: a `WWW-Authenticate` value whose scheme is `Bearer` (case-insensitive) is
   parsed into auth-params (`name=value` or `name="quoted value"`, comma separated, quoted
   strings may contain commas and backslash escapes). `scope` is split on spaces into a list
   (empty entries dropped). `error`, `error_description`, `resource_metadata` are kept if present.
   A non-Bearer challenge, or one without parameters, yields nothing.
2. A step-up is required iff the response status is **403** and the challenge parses with
   `error="insufficient_scope"` or (lenient) with a non-empty `scope`. 401 never triggers a step-up.
3. Scope merge: union of the current requested scopes and the challenge scopes, keeping the
   first-seen order and dropping duplicates.
4. The provider keeps the merged ("escalated") scopes in memory; the effective scope set for
   authorization URLs and client-credentials requests is configured scopes ∪ escalated scopes,
   in that order. The dynamic client registration body is unchanged by escalation.
5. On a step-up the HTTP client:
   - merges the challenge scopes into the provider;
   - if an **auth challenge handler** is bound (neutral hook, see 4.6): asks it to re-authorize
     with the required scopes; on success asks it to renew the connection and retries the
     request on the renewed client; on failure of either, does not retry with the old token and
     falls through to §5;
   - if no handler is bound but a refresh manager is: retries the request once on the same
     client.
   A step-up is attempted at most once per request.
6. Hook (proposal): `pub trait McpAuthChallengeHandler: Send + Sync { fn reauthorize(&self,
   scopes: Vec<String>) -> BoxFuture<Result<(), McpError>>; fn renew(&self) ->
   BoxFuture<Result<Arc<McpClient>, McpError>>; }` with `McpClient::set_auth_challenge_handler`,
   carried through `McpTransportConnection` and `ServerConnection` like the refresh manager is.
   Production wiring (service connections): `reauthorize` runs the interactive loopback
   authorization with the merged scopes, **forced** (it runs even when a fresh token is stored),
   opening the browser and posting a UI notice with the URL; `renew` renews the
   `ServerConnection` and returns its new client. The browser opener is injectable for tests.
7. Retries across renewed clients share one attempt budget of **3** per logical request; when it
   is exhausted the original failure is returned.

## 5. Refresh after rejection

On a 401 or 403 (after §4 did not succeed), if a refresh manager is bound and no forced refresh
was attempted yet for this request: force a token refresh (ignore the "still fresh" shortcut); on
success retry once; otherwise fail. The error for a final non-2xx response keeps kind `Auth` for
401 and `Protocol` otherwise, and its cause carries `status`, plus `wwwAuthenticate` (raw header)
and `requiredScopes` (parsed challenge scopes) when present.

## 6. Process-wide refresh coalescing

Refreshes are coalesced per **server URL across all refresh managers in the process** (two
managers for the same URL share one token request; different URLs run independently). The shared
work keeps running if the first caller is dropped; the entry is removed when the work settles so
a later call starts a new refresh. Public observers: "is a refresh in progress for URL" and
"number of refreshes in progress".

## 7. Secret redaction in transport errors

Any error text produced from an HTTP send/receive failure has every `http://` / `https://` token
(ending at whitespace or one of `"'<>)}]`) rewritten: query parameters whose key contains `key`,
`token` or `secret` (case-insensitive) get the value `***REDACTED***`; other parameters and URLs
without a query are unchanged; non-URLs are unchanged.

## 8. Environment for stdio servers

The host session environment passed to a stdio server (and to the connect-failure diagnosis
re-run) is filtered before the server's own configured `env` is laid over it. Dropped keys
(case-insensitive): package-manager configuration (`NPM_CONFIG_*`, `YARN_*`, `PNPM_*`,
`NO_UPDATE_NOTIFIER`), cloud/provider credentials and config (`AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `AZURE_*`, `GCP_*`, `GOOGLE_APPLICATION_CREDENTIALS`,
`GOOGLE_CLOUD_PROJECT`, `FIREBASE_*`, `HEROKU_*`, `VAULT_*`, `DOCKER_AUTH*`, `KUBECONFIG`,
`DATABASE_URL`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GITHUB_TOKEN`) and any key ending in
`_KEY`, `_SECRET`, `_TOKEN`, `_PASSWORD`, `_CREDENTIAL`, `_CREDENTIALS` or `_API_KEY`. The
server's configured `env` is applied after filtering and is never filtered. The existing base
allowlist (HOME, PATH, ...) behavior is unchanged. (The list is an interface fact: users rely on
which variables reach their servers.)

## 9. Log file sink retry

After a file write failure the logger records the existing warning line in the ring buffer and
skips the file for **5 seconds**; the first log call after that tries the file again (and on
another failure waits another 5 s). The ring buffer always receives every line.

## 10. Test cases (write new tests; no fixtures from elsewhere)

1. Resource normalization table from §1.1; form replacement/append from §1.3; `resource` present
   on authorize URL, code exchange, refresh and client credentials (axum fixture server).
2. Discovery: origin protected-resource path used for a server URL with a path; issuer origin
   retry on 404; missing `authorization_servers`; protected resource 500 reports 500; missing
   `token_endpoint`; 404 protected resource falls back to the server URL as issuer.
3. Cache: two sequential discoveries → one request per endpoint; N concurrent discoveries gated
   on a semaphore → one request; failure then success → second call refetches; reset forces a
   refetch; an injected discover runs even with a warm cache; legacy `discoveryState` record is
   dropped on read and not rewritten, unknown keys kept.
4. HTTPS: each label's error message with require-HTTPS on; loopback http allowed with it off;
   default is on.
5. Challenge parsing: quoted scopes, multiple params, non-Bearer → none; 403+insufficient_scope →
   required; 401 → not; 403 without header → not.
6. Step-up with a fake handler: one reauthorize with merged scopes, one renew, request retried on
   the renewed client, effective scopes merged; failing reauthorize → no retry with stale token;
   attempt budget of 3 bounds a server that always answers 403.
7. Interactive production handler against a loopback IdP fixture (injected browser opener that
   follows the redirect): forced login even with a fresh stored token, new token stored,
   connection renewed.
8. Refresh after rejection: 401 → one forced refresh then retry; second 401 → failure with
   `status` in the cause; 403 with challenge → cause has `wwwAuthenticate` and `requiredScopes`.
9. Coalescing: two managers, same URL → one token request, observer true while gated, false after;
   two URLs → independent; dropping the first caller does not cancel the shared refresh.
10. Redaction examples from §7.
11. stdio env: a stdio fixture server reports its env; filtered keys absent, allowed keys present,
    configured `env` keys present even if they match a filter.
12. Log sink: an unwritable path logs the warning once, skips writes inside the window and retries
    after it (drive time through an injectable clock, not sleeps).

Tests await events with bounded timeouts; no fixed sleeps.

## Leak self-check

- No code from the SUL tree; the only code-like content is the hook signature proposal (new).
- Constants that are interface facts: the HTTPS error labels, the resource normalization rule
  (from RFC 8707/MCP), the env key list (§8, marked as an interface fact), 403/401 handling (RFC
  6750/MCP), 3 attempts and 5 s (behavioral parameters).
- No OmO-original names; the hook is named `McpAuthChallengeHandler`.
