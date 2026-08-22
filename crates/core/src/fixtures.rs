// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Test/product fixtures. The `shop` fixture graph mirrors the sqlite
//! fixture database (`tests/fixtures/shop.db`), powering Phase 5 driver
//! tests and local dogfooding.

use querora_contracts::{
    semantic::*, AnalyticalQuery, DimensionRef, Filter, FilterOp, FilterValue, MeasureRef,
    OrderDirection, OrderSpec, SemanticGraph, SourceId, TimeGrain, TimeRange, TimeSpec, TimeUnit,
};
use std::collections::BTreeMap;

/// The canonical shop fixture DDL (sqlite dialect; mirrors `shop_graph`).
pub const SHOP_DDL: &str = r#"
CREATE TABLE customers (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  country TEXT,
  plan TEXT,
  created_at TEXT
);
CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  customer_id INTEGER REFERENCES customers(id),
  status TEXT,
  amount_total REAL,
  order_date TEXT
);
INSERT INTO customers (id, name, country, plan, created_at) VALUES
  (1, 'Ada', 'DK', 'pro', '2026-01-05'),
  (2, 'Linus', 'US', 'free', '2026-01-20'),
  (3, 'Grace', 'UK', 'enterprise', '2026-02-01');
INSERT INTO orders (id, customer_id, status, amount_total, order_date) VALUES
  (1, 1, 'paid', 120.0, '2026-03-02'),
  (2, 1, 'paid', 80.5, '2026-04-11'),
  (3, 2, 'refunded', 60.0, '2026-04-19'),
  (4, 3, 'paid', 300.0, '2026-05-01'),
  (5, 3, 'pending', 45.0, '2026-06-15'),
  (6, 2, 'paid', 99.9, '2026-06-30');
"#;

/// The canonical demo IR: monthly revenue for paid orders, last 6 months.
pub fn revenue_by_month_query() -> AnalyticalQuery {
    AnalyticalQuery {
        source: SourceId::new("shop"),
        measures: vec![MeasureRef {
            metric_id: "revenue".into(),
            alias: None,
        }],
        dimensions: vec![DimensionRef {
            dimension_id: "order_date".into(),
            grain: Some(TimeGrain::Month),
            alias: None,
        }],
        filters: vec![Filter {
            dimension_id: "order_status".into(),
            op: FilterOp::Eq,
            value: Some(FilterValue::Str("paid".into())),
        }],
        time: Some(TimeSpec {
            dimension_id: "order_date".into(),
            range: TimeRange::Last {
                count: 6,
                unit: TimeUnit::Month,
            },
            compare: None,
        }),
        order: vec![OrderSpec {
            key: "order_date".into(),
            direction: OrderDirection::Asc,
        }],
        limit: 100,
    }
}

/// The canonical fixture: a small e-commerce schema
/// (`customers`, `orders`) with revenue/status/time semantics.
pub fn shop_graph() -> SemanticGraph {
    let entities = BTreeMap::from([
        (
            "customers".to_string(),
            Entity {
                id: "customers".into(),
                label: "Customers".into(),
                table: "customers".into(),
                description: Some("Registered customers".into()),
                definition_sql: None,
            },
        ),
        (
            "orders".to_string(),
            Entity {
                id: "orders".into(),
                label: "Orders".into(),
                table: "orders".into(),
                description: Some("Placed orders (fact table)".into()),
                definition_sql: None,
            },
        ),
    ]);

    let metrics = BTreeMap::from([
        (
            "revenue".to_string(),
            Metric {
                id: "revenue".into(),
                label: "Revenue".into(),
                entity_id: "orders".into(),
                expr: MetricExpr {
                    op: AggOp::Sum,
                    column: Some("amount_total".into()),
                    human_formula: Some("SUM(orders.amount_total)".into()),
                    combination: None,
                },
                aliases: vec!["net revenue mrr".into(), "sales".into()],
                description: Some("Gross revenue from paid orders".into()),
            },
        ),
        (
            "order_count".to_string(),
            Metric {
                id: "order_count".into(),
                label: "Order Count".into(),
                entity_id: "orders".into(),
                expr: MetricExpr {
                    op: AggOp::Count,
                    column: None,
                    human_formula: Some("COUNT(*)".into()),
                    combination: None,
                },
                aliases: vec!["orders placed".into(), "number of orders".into()],
                description: Some("Count of orders".into()),
            },
        ),
        (
            "avg_order_value".to_string(),
            Metric {
                id: "avg_order_value".into(),
                label: "Average Order Value".into(),
                entity_id: "orders".into(),
                expr: MetricExpr {
                    op: AggOp::Avg,
                    column: Some("amount_total".into()),
                    human_formula: Some("AVG(orders.amount_total)".into()),
                    combination: None,
                },
                aliases: vec!["aov".into(), "average basket".into()],
                description: Some("Mean order value".into()),
            },
        ),
    ]);

    let dimensions = BTreeMap::from([
        (
            "order_date".to_string(),
            Dimension {
                id: "order_date".into(),
                label: "Order Date".into(),
                entity_id: "orders".into(),
                column: "order_date".into(),
                data_type: SemanticDataType::Temporal,
                aliases: vec!["order month".into(), "order time".into()],
                description: None,
            },
        ),
        (
            "order_status".to_string(),
            Dimension {
                id: "order_status".into(),
                label: "Order Status".into(),
                entity_id: "orders".into(),
                column: "status".into(),
                data_type: SemanticDataType::String,
                aliases: vec![],
                description: Some("paid | refunded | pending".into()),
            },
        ),
        (
            "customer_country".to_string(),
            Dimension {
                id: "customer_country".into(),
                label: "Customer Country".into(),
                entity_id: "customers".into(),
                column: "country".into(),
                data_type: SemanticDataType::String,
                aliases: vec!["country".into()],
                description: None,
            },
        ),
        (
            "customer_plan".to_string(),
            Dimension {
                id: "customer_plan".into(),
                label: "Customer Plan".into(),
                entity_id: "customers".into(),
                column: "plan".into(),
                data_type: SemanticDataType::String,
                aliases: vec!["plan".into(), "tier".into()],
                description: Some("free | pro | enterprise".into()),
            },
        ),
    ]);

    let relationships = vec![Relationship {
        id: "orders_to_customer".into(),
        from_entity: "orders".into(),
        from_column: "customer_id".into(),
        to_entity: "customers".into(),
        to_column: "id".into(),
        cardinality: JoinCardinality::ManyToOne,
        confidence: Confidence::Declared,
        join_kind: JoinKind::Join,
    }];

    SemanticGraph {
        source: SourceId::new("shop"),
        version: "fixture-v1".into(),
        published: true,
        entities,
        metrics,
        dimensions,
        relationships,
        value_index: Default::default(),
    }
}
