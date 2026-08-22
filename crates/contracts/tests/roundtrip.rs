// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Serialization round-trip smoke tests: serde JSON ↔ generated TypeScript
//! must never drift apart silently (risk mitigation from the plan).

use querora_contracts::*;
use std::collections::BTreeMap;

fn fixture_graph() -> SemanticGraph {
    let entity = |id: &str, table: &str| {
        (
            id.to_string(),
            Entity {
                id: id.to_string(),
                label: id.to_string(),
                table: table.to_string(),
                description: None,
                definition_sql: None,
            },
        )
    };
    SemanticGraph {
        source: SourceId::new("shop"),
        version: "v1".to_string(),
        published: true,
        entities: BTreeMap::from([entity("orders", "orders"), entity("customers", "customers")]),
        metrics: BTreeMap::from([(
            "revenue".to_string(),
            Metric {
                id: "revenue".to_string(),
                label: "Revenue".to_string(),
                entity_id: "orders".to_string(),
                expr: semantic::MetricExpr {
                    op: semantic::AggOp::Sum,
                    column: Some("amount_total".to_string()),
                    human_formula: Some("SUM(orders.amount_total)".to_string()),
                    combination: None,
                },
                aliases: vec!["sales".to_string()],
                description: Some("Gross revenue".to_string()),
            },
        )]),
        dimensions: BTreeMap::from([(
            "order_month".to_string(),
            Dimension {
                id: "order_month".to_string(),
                label: "Order Month".to_string(),
                entity_id: "orders".to_string(),
                column: "order_date".to_string(),
                data_type: SemanticDataType::Temporal,
                aliases: vec![],
                description: None,
            },
        )]),
        relationships: vec![semantic::Relationship {
            id: "orders_customer".to_string(),
            from_entity: "orders".to_string(),
            from_column: "customer_id".to_string(),
            to_entity: "customers".to_string(),
            to_column: "id".to_string(),
            cardinality: semantic::JoinCardinality::ManyToOne,
            confidence: semantic::Confidence::Declared,
            join_kind: semantic::JoinKind::Join,
        }],
        value_index: Default::default(),
    }
}

fn fixture_query() -> AnalyticalQuery {
    AnalyticalQuery {
        source: SourceId::new("shop"),
        measures: vec![MeasureRef {
            metric_id: "revenue".to_string(),
            alias: None,
        }],
        dimensions: vec![DimensionRef {
            dimension_id: "order_month".to_string(),
            grain: Some(TimeGrain::Month),
            alias: None,
        }],
        filters: vec![Filter {
            dimension_id: "order_status".to_string(),
            op: FilterOp::Eq,
            value: Some(FilterValue::Str("paid".to_string())),
        }],
        time: Some(TimeSpec {
            dimension_id: "order_month".to_string(),
            range: TimeRange::Last {
                count: 6,
                unit: TimeUnit::Month,
            },
            compare: None,
        }),
        order: vec![OrderSpec {
            key: "order_month".to_string(),
            direction: OrderDirection::Asc,
        }],
        limit: 100,
    }
}

#[test]
fn analytical_query_round_trips() {
    let q = fixture_query();
    let json = serde_json::to_string(&q).unwrap();
    let back: AnalyticalQuery = serde_json::from_str(&json).unwrap();
    assert_eq!(back.source, q.source);
    assert_eq!(back.measures.len(), 1);
    assert_eq!(back.dimensions[0].grain, Some(TimeGrain::Month));
    // tag/content enums serialize in the shape the TS types declare
    assert!(json.contains(r#""type":"last""#));
    assert!(json.contains(r#""kind":"str""#));
}

#[test]
fn semantic_graph_round_trips() {
    let g = fixture_graph();
    let json = serde_json::to_string(&g).unwrap();
    let back: SemanticGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(back.version, "v1");
    assert!(back.published);
    assert_eq!(back.metrics.len(), 1);
    assert_eq!(back.relationships.len(), 1);
}

#[test]
fn tool_error_serializes_structured() {
    let err = ToolError::new(ErrorCode::UnknownMetric, "metric `foo` not found")
        .with_details(serde_json::json!({ "known_metrics": ["revenue", "order_count"] }));
    let json = serde_json::to_string(&err).unwrap();
    assert!(json.contains("unknown_metric"));
    assert!(json.contains("known_metrics"));
}
