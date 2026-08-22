# Security Policy

## Reporting a vulnerability

Email the maintainer via the GitHub *Security* tab ("Report a
vulnerability") or open a private security advisory on this repository.
Please do not open public issues for suspected vulnerabilities.

We aim to respond within 72 hours.

## Security model (what we consider in scope)

Querora runs LLM agents against your databases under strict constraints.
The invariants below are load-bearing; reports weakening them are welcome:

- Agents receive tool access + semantic context only — they emit
  analytical IR, never SQL. Querora compiles and parameterizes.
- Credentials never enter agent context: they live in the user keychain
  (release) or 0600 files under `~/.querora/run` (dev fallback).
- The tool API is a unix socket in a 0700 directory with token
  handshake (constant-time compare); unauthenticated peers are rejected
  and audited.
- All executed SQL passes a single-SELECT read-only guard; row caps and
  timeouts are always applied.
- Agent-facing query results are truncated (≤ 50 rows + stats);
  full rows stay app-side.

## Scope notes

- The macOS beta toolchain's release-linker issue documented in
  `.cargo/config.toml` is a build-environment bug, not a Querora
  vulnerability.
- Prompt-injection via data returned from your own databases is a known
  research area; Querora mitigates by scoping agents to Querora tools
  only (least privilege) and never exposing credentials to them.
