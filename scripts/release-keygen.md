# Release signing key (minisign)

Release archives are signed with minisign. `mhc update` and the installers verify every archive against the
public key `mhc-release.pub` (repo root), which `crates/maho-cli/build.rs` embeds at build time. There is no
runtime override of the trust root.

## One-time provisioning (done; repeat only to rotate)

```sh
mkdir -p ~/.config/mhc-release && chmod 700 ~/.config/mhc-release
openssl rand -base64 24 | tr -d '\n' > ~/.config/mhc-release/password
chmod 600 ~/.config/mhc-release/password
minisign -G -p mhc-release.pub -s ~/.config/mhc-release/mhc-release.key   # enter the password from the file
chmod 600 ~/.config/mhc-release/mhc-release.key
gh secret set MINISIGN_SECRET_KEY --repo Indosaram/mhc < ~/.config/mhc-release/mhc-release.key
gh secret set MINISIGN_PASSWORD   --repo Indosaram/mhc < ~/.config/mhc-release/password
```

Commit only `mhc-release.pub`. The secret key and password stay in `~/.config/mhc-release/` (the owner's backup)
and in the repository secrets `MINISIGN_SECRET_KEY` / `MINISIGN_PASSWORD`; never print them or write them into the repo.

## Rotation

1. Generate a new pair as above and replace `mhc-release.pub`.
2. Update both repository secrets.
3. Update the pinned key literals in `install.sh` and `install.ps1`.
4. Ship the release. Binaries built before the rotation keep trusting the old key, so publish a final release
   signed with the old key that embeds the new one before retiring the old secret.

## Builds and tests

- Release builds (`MHC_BUILD_VERSION` set) fail unless a key is embedded and it is not the test fixture.
- Candidate/QA builds set `MHC_RELEASE_CHANNEL=candidate` and
  `MHC_RELEASE_PUBKEY_FILE=crates/maho-cli/tests/fixtures/release-test-key/test.pub`; they report `channel: candidate`
  in `mhc --version --verbose`. The fixture pair under `crates/maho-cli/tests/fixtures/release-test-key/` is a
  non-production, passwordless test key and is never trusted by release builds.
- Run the update integration tests (`tests/native_update_*.rs`) with
  `MHC_RELEASE_PUBKEY_FILE=crates/maho-cli/tests/fixtures/release-test-key/test.pub` in the environment; do not put it in
  `.cargo/config.toml`.
