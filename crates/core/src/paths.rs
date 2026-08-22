// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Canonical user-owned paths (`~/.querora/**`).

use std::path::PathBuf;

/// `~/.querora` (app home).
pub fn home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".querora")
}

/// `~/.querora/run` (0700): toolapi socket, tokens, dev secrets file.
pub fn run_dir() -> PathBuf {
    home().join("run")
}
