# Terminal access (dual mode)

Querora's MCP server can also serve **your own terminal agents** — Claude
Code, Codex CLI — not just the in-app chat. External processes authenticate
with a per-user token; everything is audited.

## Enable

Settings → Terminal access → **Enable (rotate token)**. This writes a fresh
256-bit token to `~/.querora/mcp-token` (0600) and shows ready-to-paste
registration snippets.

## Register with your agent

**Claude Code** (one command, consent implied by running it):

```sh
claude mcp add querora --env QUERORA_DUAL_TOKEN=<token> -- /path/to/querora-mcp
```

**Codex CLI** — merge into `~/.codex/config.toml` (Querora never writes
your config silently; copy from the snippet the app shows):

```toml
[mcp_servers.querora]
command = "/path/to/querora-mcp"
args = []
env = { QUERORA_DUAL_TOKEN = "<token>" }
requires_approval = false
```

The shim binary lives next to the Querora app binary (Contents/MacOS) and
on dev machines under `target/{debug,release}/querora-mcp`.

## What external agents can do

Exactly the in-app tool surface — `search_semantics`, `get_schema`,
`profile_column`, `execute_query`, `dry_run` — under the same guarantees:
agents emit IR, never SQL; credentials never leave the Keychain; results
are truncated (≤ 50 rows) before entering agent context.

## Security notes

- Token file is 0600, user-owned; rotate or disable any time in Settings.
- Every dual-mode authentication and tool call lands in the audit log
  (Settings shows recent connections).
- `requires_approval = false` applies to the **querora server only**; your
  other MCP servers keep their approval defaults.
