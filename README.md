# Querora

Local-first conversational BI for macOS. Ask questions in natural language; the
AI reasoning is done by **the CLI agent you already pay for** — Claude Code,
Codex CLI, or pi — driven headlessly. Querora never requires an API key.

```
ask → agent plans → validated IR → compiled SQL → chart + trust panel
```

- **BYO-agent, not BYO-key** — $0 marginal AI cost, zero AI onboarding
- **No Cube, no server, no Docker** — single Tauri binary + SQLite + Keychain
- **Human-reviewable semantic layer** — no blind text-to-SQL; the agent emits
  IR, never SQL, and credentials never cross the agent boundary
- **Dual mode** — Querora's MCP server is also usable from your own terminal
  agent sessions

## Dev quickstart

Requirements: Rust 1.97+, Node 20+, pnpm 10, Xcode CLT (macOS).

```sh
pnpm install
pnpm hooks:install        # optional: fmt + clippy pre-commit hook
pnpm tauri dev            # opens the app window
pnpm check                # typecheck + tests + frontend build
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Layout

```
apps/desktop     Tauri v2 app (React/TS frontend + Rust shell)
apps/sidecar-pi  pi SDK sidecar (agent driver + BYOK provider host)
crates/contracts serde IR contracts — single source of truth (TS exports via ts-rs)
crates/core      storage · keyring · toolapi · connectors · compiler · semantic · agents
crates/mcp       querora-mcp — MCP stdio shim bridging Claude Code / Codex to the toolapi socket
```

## Security stance

- Credentials live in the macOS Keychain, never in SQLite, never in agent context
- Agents receive tool access + semantic context only; they emit IR, never SQL
- The tool API is a token-authed unix socket at `~/.querora/run/querora.sock`

## Roadmap

**Now (v0.1 — beta)**
- [x] Chat → validated IR → compiled SQL → chart, end-to-end, via your CLI agent
- [x] BI primitives: metric arithmetic (margin %), period-over-period comparison, semi-join filters without fan-out
- [x] Sources: SQLite, DuckDB (Parquet/CSV), PostgreSQL, MySQL — read-only guarded
- [x] Semantic layer: heuristic drafts, FTS5 search, optional AI enrichment, immutable publish; Magento-style EAV schemas detected and unfolded
- [x] Dual mode: Querora's MCP server in your own terminal agents (claude/codex)
- [x] Trust panel: IR → SQL → params → row counts, CSV export
- [ ] Notarized signed DMG (pending Apple signing secrets; unsigned beta builds available)
- [ ] In-app consent flow for Codex unsandboxed mode (current codex requirement)

**Next (v0.x)**
- [ ] Semantic editor UI: hand-edit metrics/aliases/relationships, version history + diff
- [ ] Dashboard views (saved answers, grid layout) — Pro candidate
- [ ] More connectors: BigQuery, Snowflake, SQL Server
- [ ] SSH tunnels for remote sources — Pro candidate
- [ ] Windows/Linux builds (architecture keeps the seam; not yet wired)

**Later (v1)**
- [ ] BYOK mode: bring your own API key (pi custom-provider sidecar) — "agent or key, your choice"
- [ ] Scheduled reports + alerts — Pro
- [ ] Embedded-semantic search upgrades (vector search when catalog sizes demand)
- [ ] Homebrew cask submission (recipe already in-repo)

Pro policy (locked): the Pro tier NEVER gates agent access, tool
execution, or query volume — it sells value-adds only (dashboards,
scheduling, exports, tunnels).

## License

[Apache-2.0](LICENSE). See [NOTICE](NOTICE) for bundled third-party
components.
