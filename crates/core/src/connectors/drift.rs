// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Catalog drift: diff two catalog snapshots (re-introspection vs the one a
//! semantic version was built from). Feeds the Phase 7 drift report UI.

use super::types::DatabaseCatalog;
use serde::{Deserialize, Serialize};

/// One drift entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum DriftEntry {
    /// Table appeared.
    TableAdded {
        /// Table name.
        table: String,
    },
    /// Table disappeared.
    TableRemoved {
        /// Table name.
        table: String,
    },
    /// Column appeared.
    ColumnAdded {
        /// Table name.
        table: String,
        /// Column name.
        column: String,
        /// Reported type.
        data_type: String,
    },
    /// Column disappeared.
    ColumnRemoved {
        /// Table name.
        table: String,
        /// Column name.
        column: String,
    },
    /// Column type or nullability changed.
    ColumnChanged {
        /// Table name.
        table: String,
        /// Column name.
        column: String,
        /// Old reported type.
        old: String,
        /// New reported type.
        new: String,
    },
}

/// A whole drift report between two catalogs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DriftReport {
    /// Entries, most structural first.
    pub entries: Vec<DriftEntry>,
}

impl DriftReport {
    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Diff `old` (what the semantic layer was built from) against `new` (fresh
/// introspection).
pub fn diff_catalogs(old: &DatabaseCatalog, new: &DatabaseCatalog) -> DriftReport {
    use std::collections::BTreeMap;
    let old_t: BTreeMap<&str, _> = old.tables.iter().map(|t| (t.name.as_str(), t)).collect();
    let new_t: BTreeMap<&str, _> = new.tables.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut entries = Vec::new();

    for name in new_t.keys() {
        if !old_t.contains_key(name) {
            entries.push(DriftEntry::TableAdded {
                table: name.to_string(),
            });
        }
    }
    for name in old_t.keys() {
        if !new_t.contains_key(name) {
            entries.push(DriftEntry::TableRemoved {
                table: name.to_string(),
            });
        }
    }
    for (name, ot) in old_t.iter() {
        let Some(nt) = new_t.get(name) else { continue };
        let oc: BTreeMap<&str, _> = ot.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        let nc: BTreeMap<&str, _> = nt.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        for (cn, c) in nc.iter() {
            match oc.get(cn) {
                None => entries.push(DriftEntry::ColumnAdded {
                    table: name.to_string(),
                    column: cn.to_string(),
                    data_type: c.data_type.clone(),
                }),
                Some(o) => {
                    let old_sig = (o.data_type.clone(), o.nullable);
                    let new_sig = (c.data_type.clone(), c.nullable);
                    if old_sig != new_sig {
                        entries.push(DriftEntry::ColumnChanged {
                            table: name.to_string(),
                            column: cn.to_string(),
                            old: format_ty(&old_sig),
                            new: format_ty(&new_sig),
                        });
                    }
                }
            }
        }
        for cn in oc.keys() {
            if !nc.contains_key(cn) {
                entries.push(DriftEntry::ColumnRemoved {
                    table: name.to_string(),
                    column: cn.to_string(),
                });
            }
        }
    }
    DriftReport { entries }
}

fn format_ty(sig: &(String, bool)) -> String {
    format!("{} (nullable: {})", sig.0, sig.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::types::{ColumnInfo, TableInfo};

    fn cat(tables: Vec<(&str, Vec<(&str, &str)>)>) -> DatabaseCatalog {
        DatabaseCatalog {
            tables: tables
                .into_iter()
                .map(|(name, cols)| TableInfo {
                    name: name.into(),
                    is_view: false,
                    columns: cols
                        .into_iter()
                        .map(|(n, t)| ColumnInfo {
                            name: n.into(),
                            data_type: t.into(),
                            nullable: true,
                            primary_key: false,
                        })
                        .collect(),
                    foreign_keys: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn drift_detects_adds_removes_changes() {
        let old = cat(vec![
            ("orders", vec![("id", "INTEGER"), ("status", "TEXT")]),
            ("customers", vec![("id", "INTEGER")]),
        ]);
        let new = cat(vec![
            (
                "orders",
                vec![("id", "BIGINT"), ("status", "TEXT"), ("amount", "REAL")],
            ),
            ("events", vec![("id", "INTEGER")]),
        ]);
        let report = diff_catalogs(&old, &new);
        let kinds: Vec<String> = report
            .entries
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(kinds.contains(&"table_added".to_string())); // events
        assert!(kinds.contains(&"table_removed".to_string())); // customers
        assert!(kinds.contains(&"column_added".to_string())); // orders.amount
        assert!(kinds.contains(&"column_changed".to_string())); // orders.id INTEGER→BIGINT
        assert!(!report.is_empty());
    }

    #[test]
    fn identical_catalogs_have_no_drift() {
        let c = cat(vec![("orders", vec![("id", "INTEGER")])]);
        assert!(diff_catalogs(&c, &c).is_empty());
    }
}
