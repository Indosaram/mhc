# Task 6 Evidence: Port webfetch Cloudflare challenge retry (T06)

## Metadata
- **Task**: 6 (T06)
- **Branch**: `lane/2006`
- **Base commit**: `b3c3c94f2cf0220a7de0d3ac1f9434461fc14c02`
- **Date**: 2026-10-09

## Summary of Changes
- Implemented Cloudflare challenge 403 retry in `crates/builtins/maho-ext-webfetch/src/webfetch/fetcher.rs`:
  - Defined `BROWSER_USER_AGENT` and `RETRY_USER_AGENT` (`"pi-webfetch"`).
  - Detected `403 Forbidden` response with `cf-mitigated: challenge` header on the initial browser request.
  - Bounded discard of the challenged response body.
  - Single retry using the pinned alternate User-Agent `pi-webfetch` (omitting Chrome client hint headers).
  - Cancellation / abort honored during the challenge retry.
  - Non-challenge 403 returns without retry (single request).
- Added comprehensive unit/integration tests in `fetcher.rs`:
  - `cloudflare_challenge_retries_with_alternate_user_agent` (happy path: exactly 2 requests, 403 cf-mitigated -> 200 retried).
  - `plain_403_without_cf_header_does_not_retry` (failure scenario: exactly 1 request, returns 403 forbidden).
  - `abort_during_challenge_retry_is_honored` (abort during retry returns `WebfetchError::Abort`).
- Added parity ledger fragment `crates/builtins/maho-ext-webfetch/parity.d/2006.md`.

## Verification Commands and Output

### 1. Parity Audit
```bash
bun tools/parity-audit.mjs --crate maho-ext-webfetch
```
- **Exit Code**: 0
- **Output**: `PASS maho-ext-webfetch: 9 files checked`

### 2. Cargo Nextest Run
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-ext-webfetch
```
- **Exit Code**: 0
- **Output**: `108 tests run: 108 passed, 0 skipped`

### 3. Cargo Clippy
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo clippy -p maho-ext-webfetch --all-targets -- -D warnings
```
- **Exit Code**: 0
- **Output**: Clean (0 warnings, 0 errors)

### 4. QA Scenarios Verification
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-ext-webfetch -E 'test(cloudflare_challenge_retries_with_alternate_user_agent) | test(plain_403_without_cf_header_does_not_retry) | test(abort_during_challenge_retry_is_honored)' --no-capture
```
- **Exit Code**: 0
- **Scenario Happy (403 cf-mitigated -> alternate UA retry 200)**: PASS (exactly 2 requests)
- **Scenario Failure (plain 403 without header -> no retry)**: PASS (exactly 1 request)
- **Scenario Abort (abort during retry honored)**: PASS (`WebfetchError::Abort`)

## Cleanup Receipt
- All test socket connections and TCP listeners were closed immediately upon test completion.
- Temp build directory: `$HOME/.cache/mhc-lanes/2006` scoped and cleaned.
- No dangling processes or leaked server sockets.
