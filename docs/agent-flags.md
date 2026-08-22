# Agent flag matrix (Phase 5 spike — 2026-08-16)

Verified against: claude 2.1.233 · codex 0.147.0 · pi 0.84.2.

## Claude Code (GO — zero friction)

| Concern | Flag | Result |
|---|---|---|
| Headless run | `claude -p --output-format stream-json --verbose` | works; events: assistant/user/result |
| MCP server | `--mcp-config <file>` | loads; stdio JSON-RPC NDJSON |
| MCP auto-approval | `--allowedTools "mcp__querora__*"` | **auto-approved headless, no prompt** |
| Least privilege | `--disallowedTools "Bash,Edit,Write,…"` | verified: agent reports NO_BASH |
| Resume | `--resume <session_id>` | verified: codeword recalled |
| Token transport | env in mcp-config file (0600, per-session, deleted after) | never argv-visible to `ps eww` |

## Codex CLI (GO — consent-gated)

| Concern | Flag | Result |
|---|---|---|
| Headless run | `codex exec --json --skip-git-repo-check -` (prompt via stdin) | works; JSONL events |
| MCP server | `-c 'mcp_servers.querora={command=…, env={…}}'` + scalar `-c 'mcp_servers.querora.requires_approval=false'` | loads |
| MCP auto-approval (sandboxed) | `--sandbox read-only` / `workspace-write` | **AUTO-CANCELLED** — `request_user_input is not supported in exec mode` elicitations resolve `Cancel` (codex 0.147; `features.default_mode_request_user_input=false` does not help) |
| MCP auto-approval (unsandboxed) | `--sandbox danger-full-access` + `requires_approval=false` + `features.default_mode_request_user_input=false` | **works** |
| Least privilege | sandbox (when sandboxed) + only querora MCP `requires_approval=false` | other servers keep approval defaults |
| Resume | `codex exec resume <thread_id>` | thread.started id observed |

**Red-line consequence:** the only working codex mode disables the sandbox.
`CodexDriver` defaults to sandboxed (surfaces consent-needed error);
`CodexDriver::unsandboxed()` requires explicit recorded user consent
(the consent dialog is the plan's pre-decided fallback).

## pi (via sidecar — no MCP by design)

- SDK: `createAgentSession({ customTools, tools: [<querora tools>], noTools: "builtin" })`
- Custom `AgentTool`s bridge the toolapi unix socket (token via `--token`,
  passed at spawn; BYOK key via fd).
- Session resume: SDK session manager; RPC fallback (`pi --mode rpc`)
  documented as plan-B if the SDK breaks.
