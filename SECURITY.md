# Security Policy

## Supported Versions

Only the latest release of mhc is supported with security updates.

| Version | Supported |
| ------- | --------- |
| Latest release | :white_check_mark: |
| Older releases | :x: |

## Reporting a Vulnerability

If you discover a security vulnerability in mhc, please report it privately through GitHub Security Advisories:

**https://github.com/Indosaram/mhc/security/advisories/new**

Please do **not** report security vulnerabilities through public GitHub issues, discussions, or pull requests.

### Response Targets

We aim to:
- Acknowledge receipt of vulnerability reports within 7 days.
- Provide a preliminary assessment and follow-up timeline after acknowledgment.
- Keep you updated as we develop and validate a security fix before disclosure.

## Scope

The following components are within scope for security reports:
- The `mhc` binary and bundled core tools
- Official installers (`install.sh`, `install.ps1`)
- The native self-update mechanism

The following are out of scope:
- Vulnerabilities in third-party LLM providers or models
- Host machine compromises outside the boundaries of mhc's tool runtime
- Unsupported older releases

## Known Risky Features

mhc includes several capabilities that interact directly with the host machine or external providers. Intentional design choices, safety defaults, and divergences from upstream are documented in [docs/divergences.md](docs/divergences.md):

- **Consent-gated providers:** Providers that impersonate first-party clients (such as Claude Code or Cursor) or perform multi-account rotation require explicit, recorded user consent prior to execution.
- **Opt-in sandbox:** Command execution tools operate under a guarded path-checking approval model by default; users can opt into OS-level write confinement (`bashSandbox`) on supported platforms (Linux and macOS) for defense-in-depth isolation.
