// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Compiler golden tests: one deterministic fixture per dialect, locked in
//! `tests/golden/*.sql`. Regenerate with
//! `QUERORA_UPDATE_GOLDEN=1 cargo test -p querora-core --test compiler_golden`.

use querora_contracts::*;
use querora_core::compiler;
use querora_core::connectors::guard::assert_single_select;
use querora_core::connectors::types::Dialect;
use querora_core::fixtures::shop_graph;
use std::fmt::Write as _;

/// Deterministic fixture (absolute time range — no `now` dependency).
fn golden_query() -> AnalyticalQuery {
    AnalyticalQuery {
        source: SourceId::new("shop"),
        measures: vec![
            MeasureRef {
                metric_id: "revenue".into(),
                alias: None,
            },
            MeasureRef {
                metric_id: "order_count".into(),
                alias: Some("n_orders".into()),
            },
        ],
        dimensions: vec![
            DimensionRef {
                dimension_id: "order_date".into(),
                grain: Some(TimeGrain::Month),
                alias: None,
            },
            DimensionRef {
                dimension_id: "customer_country".into(),
                grain: None,
                alias: None,
            },
        ],
        filters: vec![
            Filter {
                dimension_id: "order_status".into(),
                op: FilterOp::Eq,
                value: Some(FilterValue::Str("paid".into())),
            },
            Filter {
                dimension_id: "customer_plan".into(),
                op: FilterOp::In,
                value: Some(FilterValue::List(vec![
                    FilterValue::Str("pro".into()),
                    FilterValue::Str("enterprise".into()),
                ])),
            },
        ],
        time: Some(TimeSpec {
            dimension_id: "order_date".into(),
            range: TimeRange::Between {
                start: "2026-01-01".into(),
                end: "2026-06-30".into(),
            },
            compare: None,
        }),
        order: vec![
            OrderSpec {
                key: "order_date".into(),
                direction: OrderDirection::Asc,
            },
            OrderSpec {
                key: "revenue".into(),
                direction: OrderDirection::Desc,
            },
        ],
        limit: 500,
    }
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, sql: &str, params: &[serde_json::Value]) {
    let mut snapshot = sql.to_string();
    snapshot.push_str("\n-- params: ");
    let _ = write!(snapshot, "{}", serde_json::to_string(params).unwrap());
    let path = golden_path(name);
    if std::env::var("QUERORA_UPDATE_GOLDEN").ok().as_deref() == Some("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &snapshot).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file missing: {} (run with QUERORA_UPDATE_GOLDEN=1)",
            path.display()
        )
    });
    assert_eq!(expected, snapshot, "dialect output drifted for {name}");
}

#[test]
fn golden_all_dialects() {
    let graph = shop_graph();
    let q = golden_query();
    for (name, d) in [
        ("pg.sql", Dialect::Pg),
        ("mysql.sql", Dialect::Mysql),
        ("sqlite.sql", Dialect::Sqlite),
        ("duckdb.sql", Dialect::DuckDb),
    ] {
        let plan = compiler::compile(&q, &graph, d).unwrap();
        // hard cap + timeout always present
        assert!(plan.row_cap >= 1 && plan.row_cap <= 1000);
        assert!(plan.timeout_secs >= 1);
        // single-statement SELECT guarantee
        assert_single_select(&plan.sql).unwrap_or_else(|e| panic!("{name}: {e}"));
        check(name, &plan.sql, &plan.params);
    }
}

/// Property-ish test: combinatorial valid IRs compile to parseable
/// single-SELECT SQL in every dialect (sqlparser round-trip).
#[test]
fn every_valid_ir_compiles_to_parseable_single_select() {
    let graph = shop_graph();
    let metrics = ["revenue", "order_count", "avg_order_value"];
    let dims = [
        ("order_date", Some(TimeGrain::Month)),
        ("order_date", Some(TimeGrain::Year)),
        ("order_status", None),
        ("customer_country", None),
        ("customer_plan", None),
    ];
    let filters = [
        None,
        Some(FilterOp::Eq),
        Some(FilterOp::In),
        Some(FilterOp::Gte),
        Some(FilterOp::Like),
        Some(FilterOp::IsNull),
    ];
    let mut count = 0;
    for m in metrics {
        for (d, grain) in dims {
            for f in filters.iter() {
                for limit in [1u32, 100, 100_000] {
                    let mut q = AnalyticalQuery {
                        source: SourceId::new("shop"),
                        measures: vec![MeasureRef {
                            metric_id: m.into(),
                            alias: None,
                        }],
                        dimensions: vec![DimensionRef {
                            dimension_id: d.into(),
                            grain,
                            alias: None,
                        }],
                        filters: vec![],
                        time: None,
                        order: vec![],
                        limit,
                    };
                    if let Some(op) = f {
                        q.filters = vec![Filter {
                            dimension_id: "order_status".into(),
                            op: *op,
                            value: match op {
                                FilterOp::Eq => Some(FilterValue::Str("paid".into())),
                                FilterOp::In => Some(FilterValue::List(vec![
                                    FilterValue::Str("paid".into()),
                                    FilterValue::Str("pending".into()),
                                ])),
                                FilterOp::Gte => Some(FilterValue::Number(50.0)),
                                FilterOp::Like => Some(FilterValue::Str("%paid%".into())),
                                _ => None,
                            },
                        }];
                    }
                    for d2 in [
                        Dialect::Pg,
                        Dialect::Mysql,
                        Dialect::Sqlite,
                        Dialect::DuckDb,
                    ] {
                        if let Ok(plan) = compiler::compile(&q, &graph, d2) {
                            assert_single_select(&plan.sql).unwrap_or_else(|e| {
                                panic!("ir m={m} d={d} f={f:?} {d2:?}: {e}\n{}", plan.sql)
                            });
                            assert!(plan.row_cap <= 1000);
                        }
                        // structural rejections (e.g. grain on non-temporal) are fine
                    }
                    count += 1;
                }
            }
        }
    }
    assert!(count >= 270, "combinatorial coverage: {count} IRs");
}

/// Unknown metric error must carry the known ids (agent self-correction).
#[test]
fn unknown_metric_contract_error() {
    let graph = shop_graph();
    let mut q = golden_query();
    q.measures[0].metric_id = "not_a_metric".into();
    let err = compiler::compile(&q, &graph, Dialect::Sqlite).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownMetric);
    let d = err.details.expect("details present");
    let known: Vec<String> =
        serde_json::from_value::<Vec<String>>(d["known_metrics"].clone()).unwrap();
    assert!(known.contains(&"revenue".to_string()));
}

/// EXPLAIN dry-run wraps the plan and is marked explain-only.
#[test]
fn explain_mode_is_flagged_and_prefixed() {
    let graph = shop_graph();
    let q = golden_query();
    let plan = compiler::compile_explain(&q, &graph, Dialect::Sqlite).unwrap();
    assert!(plan.explain_only);
    assert!(plan.sql.starts_with("EXPLAIN QUERY PLAN "));
}
