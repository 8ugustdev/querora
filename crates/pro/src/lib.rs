// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! # querora-pro — Pro feature gate (STUB)
//!
//! Policy (locked, red-team #9): **Pro NEVER gates agent access, tool
//! execution, or query volume.** Pro sells value-adds only (dashboards,
//! scheduled reports, exports, SSH tunnels). This crate is the single
//! seam where such checks would live; in the OSS build `is_pro()` is
//! always `true` (nothing gated). Before any Pro launch: review the
//! agent-subscription ToS of claude/codex/pi.

/// Whether Pro features are unlocked. OSS build: always true (no gating).
pub fn is_pro() -> bool {
    true
}

/// Future Pro-gated feature ids (none active in M0).
pub const PRO_FEATURES: &[&str] = &[
    "dashboards",
    "scheduled_reports",
    "bulk_exports",
    "ssh_tunnels",
];

/// Check a feature. OSS: everything allowed (stub behavior documented).
pub fn feature_enabled(_feature: &str) -> bool {
    is_pro()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oss_build_gates_nothing() {
        assert!(is_pro());
        for f in PRO_FEATURES {
            assert!(feature_enabled(f), "M0: nothing may be gated");
        }
    }
}
