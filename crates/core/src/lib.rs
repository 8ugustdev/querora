// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! # querora-core
//!
//! The Rust core of Querora: storage (SQLite app db), keyring (macOS
//! Keychain), toolapi (token-authed unix-socket JSON-RPC tool surface),
//! connectors (Postgres/MySQL/SQLite/DuckDB), compiler (IR → validated SQL),
//! semantic layer (heuristics + FTS5 retrieval), and agent drivers.
//!
//! Layering rule: everything in this crate consumes types from
//! `querora-contracts`; nothing in `contracts` consumes this crate.

pub mod agents;
pub mod compiler;
pub mod connectors;
pub mod dualmode;
pub mod fixtures;
pub mod keyring;
pub mod paths;
pub mod semantic;
pub mod storage;
pub mod toolapi;

pub use keyring::{CredentialStore, KeychainStore, MemoryStore};
pub use storage::AppStore;
