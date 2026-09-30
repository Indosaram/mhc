//! Port of senpi packages/ai/src/cli.ts: the `npx @earendil-works/pi-ai` OAuth login CLI.
//!
//! The TS file's entire body is interactive OAuth glue over `providers/all.ts`'s
//! `builtinProviders()` and each provider's `auth.oauth.login()` (auth/types.ts `AuthPrompt`,
//! `OAuthCredential`). Those types and the provider registry are filled in by todos 10-13;
//! senpi has no unit test targeting this file (see packages/ai/test), so there is nothing to
//! port here yet beyond this module placeholder.
