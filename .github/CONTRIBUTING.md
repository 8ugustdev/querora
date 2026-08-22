# Contributing to Querora

Thanks for your interest!

## Dev quickstart

Requirements: macOS, Rust 1.97+, Node 20+, pnpm 10, Xcode CLT.

```sh
pnpm install
pnpm hooks:install
pnpm tauri dev            # app window + hot reload
cargo test --workspace    # rust tests
pnpm test                 # frontend tests
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
```

## Ground rules

- **Security model is not negotiable**: agents emit IR, never SQL;
  credentials stay in the user's keychain (or 0600 files in dev);
  tool payloads are truncated; the read-only guard stays.
  Changes weakening these will be rejected.
- New connector code lives behind the `DataSource` trait; nothing outside
  `connectors/` imports a driver crate.
- Contract changes (`crates/contracts`) require regenerated TS types
  (`cargo test -p querora-contracts`) and must be committed together.
- Compiler changes need golden-file coverage
  (`QUERORA_UPDATE_GOLDEN=1 cargo test -p querora-core --test compiler_golden`).
- CI is **manual-only** (quota control). Run it before opening a PR:
  `gh workflow run CI --repo <fork>`.

## Commit style

Conventional commits (`feat:`, `fix:`, `docs:`, `chore:` …). Keep commits
small and reviewable.

## Reporting issues

Bugs → GitHub Issues with repro steps. Security → see SECURITY.md
(please do not open public issues for vulnerabilities).
