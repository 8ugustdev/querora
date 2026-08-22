// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Heuristic semantic-draft suggester: catalog → draft `SemanticGraph`
//! with NO AI. Sources of evidence, in confidence order:
//! 1. declared PKs → entity identity
//! 2. declared FKs → relationships (`Declared` confidence)
//! 3. naming conventions (`foo_id` ↔ `foo.id`) → candidate relationships
//!    (`Candidate` confidence — flagged for human review)
//! 4. numeric columns → measure candidates (sum/avg) + `<Entity> Count`
//! 5. categorical/temporal columns → dimensions
//!
//! Also emits an "unjoined tables" report so coverage gaps are visible
//! instead of silent (red-team finding #3).

use crate::connectors::types::DatabaseCatalog;
use querora_contracts::semantic::{
    AggOp, Confidence, Dimension, Entity, JoinCardinality, JoinKind, Metric, MetricExpr,
    Relationship, SemanticDataType, SemanticGraph,
};
use querora_contracts::SourceId;
use std::collections::{BTreeMap, HashSet};

/// Suggester output: the draft graph + review items.
#[derive(Debug)]
pub struct Suggestion {
    /// Draft semantic graph (published=false).
    pub graph: SemanticGraph,
    /// Tables no relationship touches — explicit coverage gap.
    pub unjoined_tables: Vec<String>,
    /// Candidate (naming-convention) relationships needing review.
    pub candidate_relationships: Vec<String>,
}

/// Infer the semantic data type of a physical column.
pub fn infer_data_type(db_type: &str) -> SemanticDataType {
    let t = db_type.to_lowercase();
    if t.contains("int") {
        SemanticDataType::Integer
    } else if t.contains("real")
        || t.contains("floa")
        || t.contains("doub")
        || t.contains("num")
        || t.contains("dec")
    {
        SemanticDataType::Number
    } else if t.contains("bool") || t == "bit(1)" {
        SemanticDataType::Boolean
    } else if t.contains("date") || t.contains("time") {
        SemanticDataType::Temporal
    } else {
        SemanticDataType::String
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_end_matches('_').to_string()
}

fn humanize(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut cs = p.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generate the heuristic draft.
pub fn suggest(source: &SourceId, catalog: &DatabaseCatalog) -> Suggestion {
    let mut entities = BTreeMap::new();
    let mut dimensions = BTreeMap::new();
    let mut metrics = BTreeMap::new();
    let mut relationships = Vec::new();
    let mut candidate_relationships = Vec::new();

    // entity per table (slug ids); remember pk columns
    let mut pk_cols: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in &catalog.tables {
        let id = slug(&t.name);
        entities.insert(
            id.clone(),
            Entity {
                id: id.clone(),
                label: humanize(&t.name),
                table: t.name.clone(),
                description: None,
                definition_sql: None,
            },
        );
        let pks: Vec<String> = t
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect();
        if !pks.is_empty() {
            pk_cols.insert(t.name.clone(), pks);
        }
    }

    // declared FKs → Declared relationships
    let mut joined_tables: HashSet<String> = HashSet::new();
    for t in &catalog.tables {
        for fk in &t.foreign_keys {
            if !entities.contains_key(&slug(&fk.ref_table)) {
                continue;
            }
            let from = slug(&t.name);
            let to = slug(&fk.ref_table);
            relationships.push(Relationship {
                id: format!("{from}_{}_{to}", fk.column),
                from_entity: from.clone(),
                from_column: fk.column.clone(),
                to_entity: to.clone(),
                to_column: fk.ref_column.clone(),
                cardinality: JoinCardinality::ManyToOne,
                confidence: Confidence::Declared,
                join_kind: JoinKind::Join,
            });
            joined_tables.insert(t.name.clone());
            joined_tables.insert(fk.ref_table.clone());
        }
    }

    // naming conventions: `<x>_id` on A ↔ PK `id` on table slug(x)
    for t in &catalog.tables {
        for c in &t.columns {
            let name = c.name.to_lowercase();
            let Some(stem) = name.strip_suffix("_id") else {
                continue;
            };
            if stem.is_empty() || name == "id" {
                continue;
            }
            // candidate target: table named exactly stem, or plural forms
            let targets: Vec<&str> = catalog
                .tables
                .iter()
                .map(|tt| tt.name.as_str())
                .filter(|tn| {
                    let s = slug(tn);
                    s == slug(stem)
                        || s == format!("{}s", slug(stem))
                        || s == format!("{}es", slug(stem))
                })
                .collect();
            for target in targets {
                let from = slug(&t.name);
                let to = slug(target);
                if from == to {
                    continue;
                }
                let dup = relationships
                    .iter()
                    .any(|r| r.from_entity == from && r.from_column == c.name && r.to_entity == to);
                if dup {
                    continue;
                }
                let to_pk = pk_cols
                    .get(target)
                    .and_then(|v| v.first())
                    .cloned()
                    .unwrap_or_else(|| "id".to_string());
                relationships.push(Relationship {
                    id: format!("{from}_{}_{}__cand", c.name, to),
                    from_entity: from,
                    from_column: c.name.clone(),
                    to_entity: to,
                    to_column: to_pk,
                    cardinality: JoinCardinality::ManyToOne,
                    confidence: Confidence::Candidate,
                    join_kind: JoinKind::Join,
                });
                candidate_relationships.push(format!(
                    "{}.{} → {target} (naming convention)",
                    t.name, c.name
                ));
                joined_tables.insert(t.name.clone());
                joined_tables.insert(target.to_string());
            }
        }
    }

    // dimensions + metrics per table
    for t in &catalog.tables {
        let entity = slug(&t.name);
        for c in &t.columns {
            if c.primary_key {
                continue;
            }
            let dt = infer_data_type(&c.data_type);
            let dim_id = format!("{entity}__{}", slug(&c.name));
            match dt {
                SemanticDataType::Integer | SemanticDataType::Number => {
                    // numeric → measure candidates
                    let base = slug(&c.name);
                    metrics.insert(
                        format!("{entity}__sum_{base}"),
                        Metric {
                            id: format!("{entity}__sum_{base}"),
                            label: format!("{} (Sum of {})", humanize(&t.name), humanize(&c.name)),
                            entity_id: entity.clone(),
                            expr: MetricExpr {
                                op: AggOp::Sum,
                                column: Some(c.name.clone()),
                                human_formula: Some(format!("SUM({}.{})", t.name, c.name)),
                                combination: None,
                            },
                            aliases: vec![humanize(&c.name).to_lowercase()],
                            description: None,
                        },
                    );
                    // IDs (fk-ish) are not useful dimensions; only true numerics
                    if !c.name.to_lowercase().ends_with("_id") {
                        dimensions.insert(
                            dim_id.clone(),
                            Dimension {
                                id: dim_id,
                                label: humanize(&c.name),
                                entity_id: entity.clone(),
                                column: c.name.clone(),
                                data_type: dt,
                                aliases: vec![],
                                description: None,
                            },
                        );
                    }
                }
                _ => {
                    dimensions.insert(
                        dim_id.clone(),
                        Dimension {
                            id: dim_id,
                            label: humanize(&c.name),
                            entity_id: entity.clone(),
                            column: c.name.clone(),
                            data_type: dt,
                            aliases: vec![],
                            description: None,
                        },
                    );
                }
            }
        }
        // entity-count metric
        metrics.insert(
            format!("{entity}__count"),
            Metric {
                id: format!("{entity}__count"),
                label: format!("{} Count", humanize(&t.name)),
                entity_id: entity.clone(),
                expr: MetricExpr {
                    op: AggOp::Count,
                    column: None,
                    human_formula: Some(format!("COUNT({}.*)", t.name)),
                    combination: None,
                },
                aliases: vec![format!("number of {}", slug(&t.name))],
                description: None,
            },
        );
    }

    let unjoined_tables: Vec<String> = catalog
        .tables
        .iter()
        .map(|t| t.name.clone())
        .filter(|n| !joined_tables.contains(n))
        .collect();

    Suggestion {
        graph: SemanticGraph {
            source: source.clone(),
            version: String::new(),
            published: false,
            entities,
            metrics,
            dimensions,
            relationships,
            value_index: Default::default(),
        },
        unjoined_tables,
        candidate_relationships,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::types::{ColumnInfo, ForeignKey, TableInfo};

    fn shop_catalog() -> DatabaseCatalog {
        DatabaseCatalog {
            tables: vec![
                TableInfo {
                    name: "customers".into(),
                    is_view: false,
                    columns: vec![
                        ColumnInfo {
                            name: "id".into(),
                            data_type: "INTEGER".into(),
                            nullable: false,
                            primary_key: true,
                        },
                        ColumnInfo {
                            name: "name".into(),
                            data_type: "TEXT".into(),
                            nullable: false,
                            primary_key: false,
                        },
                        ColumnInfo {
                            name: "country".into(),
                            data_type: "TEXT".into(),
                            nullable: true,
                            primary_key: false,
                        },
                        ColumnInfo {
                            name: "plan".into(),
                            data_type: "TEXT".into(),
                            nullable: true,
                            primary_key: false,
                        },
                    ],
                    foreign_keys: vec![],
                },
                TableInfo {
                    name: "orders".into(),
                    is_view: false,
                    columns: vec![
                        ColumnInfo {
                            name: "id".into(),
                            data_type: "INTEGER".into(),
                            nullable: false,
                            primary_key: true,
                        },
                        ColumnInfo {
                            name: "customer_id".into(),
                            data_type: "INTEGER".into(),
                            nullable: true,
                            primary_key: false,
                        },
                        ColumnInfo {
                            name: "status".into(),
                            data_type: "TEXT".into(),
                            nullable: true,
                            primary_key: false,
                        },
                        ColumnInfo {
                            name: "amount_total".into(),
                            data_type: "REAL".into(),
                            nullable: true,
                            primary_key: false,
                        },
                        ColumnInfo {
                            name: "order_date".into(),
                            data_type: "TEXT".into(),
                            nullable: true,
                            primary_key: false,
                        },
                    ],
                    foreign_keys: vec![ForeignKey {
                        name: "orders_customer_fkey".into(),
                        column: "customer_id".into(),
                        ref_table: "customers".into(),
                        ref_column: "id".into(),
                    }],
                },
                TableInfo {
                    name: "events".into(),
                    is_view: false,
                    columns: vec![ColumnInfo {
                        name: "id".into(),
                        data_type: "INTEGER".into(),
                        nullable: false,
                        primary_key: true,
                    }],
                    foreign_keys: vec![],
                },
            ],
        }
    }

    #[test]
    fn declared_fk_beats_naming_convention() {
        let s = suggest(&SourceId::new("shop"), &shop_catalog());
        let declared = s
            .graph
            .relationships
            .iter()
            .filter(|r| r.from_entity == "orders" && r.confidence == Confidence::Declared)
            .count();
        assert_eq!(declared, 1, "FK is declared, not duplicated by convention");
        assert!(!s
            .graph
            .relationships
            .iter()
            .any(|r| r.confidence == Confidence::Candidate && r.from_entity == "orders"));
    }

    #[test]
    fn naming_convention_creates_candidates() {
        let mut cat = shop_catalog();
        cat.tables[1].foreign_keys.clear(); // strip the FK → convention must find it
        let s = suggest(&SourceId::new("shop"), &cat);
        assert!(s
            .graph
            .relationships
            .iter()
            .any(|r| r.confidence == Confidence::Candidate));
        assert!(s
            .candidate_relationships
            .iter()
            .any(|c| c.contains("customer_id")));
    }

    #[test]
    fn unjoined_tables_are_reported() {
        let s = suggest(&SourceId::new("shop"), &shop_catalog());
        assert_eq!(
            s.unjoined_tables,
            vec!["events"],
            "coverage gap must be explicit"
        );
    }

    #[test]
    fn draft_has_measures_dimensions_and_counts() {
        let s = suggest(&SourceId::new("shop"), &shop_catalog());
        assert!(!s.graph.published);
        assert!(s.graph.metrics.contains_key("orders__sum_amount_total"));
        assert!(s.graph.metrics.contains_key("orders__count"));
        assert!(s.graph.dimensions.contains_key("orders__order_date"));
        assert!(
            !s.graph.dimensions.contains_key("orders__customer_id"),
            "fk-ish numeric not a dimension"
        );
        assert!(s.graph.dimensions.contains_key("customers__country"));
    }
}
