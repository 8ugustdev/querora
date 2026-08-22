// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Connector tests. SQLite + DuckDB run everywhere; Postgres/MySQL are
//! env-gated (`QUERORA_IT_PG` / `QUERORA_IT_MYSQL` with a DSN) and run in
//! CI via service containers.

use querora_contracts::{SourceId, SourceInfo, SourceKind};
use querora_core::connectors::{self, RowCap};
use rand::RngCore;
use std::path::PathBuf;
use std::str::FromStr;

/// The canonical shop fixture schema (mirrors `fixtures::shop_graph`).
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

fn temp_path(ext: &str) -> PathBuf {
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    let name: String = b.iter().map(|x| format!("{x:02x}")).collect();
    std::env::temp_dir().join(format!("querora-conn-{name}.{ext}"))
}

async fn sqlite_fixture() -> SourceInfo {
    let path = temp_path("db");
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
    sqlx::raw_sql(SHOP_DDL).execute(&pool).await.unwrap();
    pool.close().await;
    SourceInfo {
        id: SourceId::new("shop"),
        name: "Shop".into(),
        kind: SourceKind::Sqlite,
        params: serde_json::json!({ "path": path.display().to_string() }),
        created_at: String::new(),
    }
}

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn sqlite_catalog_profiles_and_executes() {
    let info = sqlite_fixture().await;
    let ds = connectors::connect(&info, "").await.unwrap();
    assert_eq!(ds.dialect(), connectors::Dialect::Sqlite);

    // catalog: tables, columns, PK/FK
    let cat = ds.catalog().await.unwrap();
    let orders = cat
        .tables
        .iter()
        .find(|t| t.name == "orders")
        .expect("orders table");
    assert!(orders.columns.iter().any(|c| c.name == "amount_total"));
    let fk = orders
        .foreign_keys
        .iter()
        .find(|f| f.column == "customer_id")
        .expect("fk");
    assert_eq!(fk.ref_table, "customers");
    assert_eq!(fk.ref_column, "id");
    let customers = cat
        .tables
        .iter()
        .find(|t| t.name == "customers")
        .expect("customers table");
    assert!(customers
        .columns
        .iter()
        .any(|c| c.name == "id" && c.primary_key));

    // profile
    let prof = ds.profile("orders", "status", 1000).await.unwrap();
    assert_eq!(prof.distinct_count, Some(3));
    assert_eq!(prof.null_ratio, Some(0.0));
    assert_eq!(prof.top_values.len(), 3);
    assert!(
        prof.top_values.iter().any(|(v, c)| v == "paid" && *c == 4),
        "top: {:?}",
        prof.top_values
    );

    // execute: params bound, cap applied (paid: 4 orders, 120+80.5+300+99.9)
    let rows = ds
        .execute(
            "SELECT status, COUNT(*) AS n, SUM(amount_total) AS total FROM orders WHERE status != ?1 GROUP BY status ORDER BY status",
            &[serde_json::json!("pending")],
            RowCap { limit: 10, timeout_secs: 5 },
        )
        .await
        .unwrap();
    assert_eq!(rows.columns, vec!["status", "n", "total"]);
    assert_eq!(rows.rows.len(), 2); // paid, refunded
    let paid = rows
        .rows
        .iter()
        .find(|r| r[0] == serde_json::json!("paid"))
        .unwrap();
    assert_eq!(paid[1], serde_json::json!(4));
    assert_eq!(paid[2], serde_json::json!(120.0 + 80.5 + 300.0 + 99.9));

    // guard: writes rejected at the connector boundary
    let err = ds
        .execute("DELETE FROM orders", &[], RowCap::default())
        .await
        .unwrap_err();
    assert!(err.message.contains("read-only guard") || err.message.contains("SELECT"));
    let err = ds
        .execute("SELECT 1; DROP TABLE orders", &[], RowCap::default())
        .await
        .unwrap_err();
    assert!(err.message.contains("statement") || err.message.contains("read-only"));
}

#[tokio::test]
async fn sqlite_row_cap_is_enforced() {
    let info = sqlite_fixture().await;
    let ds = connectors::connect(&info, "").await.unwrap();
    let rows = ds
        .execute(
            "SELECT id FROM orders",
            &[],
            RowCap {
                limit: 2,
                timeout_secs: 5,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.row_cap, 2);
}

#[cfg(feature = "duckdb")]
#[tokio::test]
async fn duckdb_parquet_source_catalog_and_query() {
    // build a parquet file from the sqlite fixture data using duckdb itself
    let sqlite_path = {
        let info = sqlite_fixture().await;
        info.params["path"].as_str().unwrap().to_string()
    };
    let pq_path = temp_path("parquet");
    {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "INSTALL sqlite; LOAD sqlite;
             ATTACH 'sqlite://{sqlite_path}' AS sq (READ_ONLY);
             COPY (SELECT * FROM sq.orders) TO '{pq}' (FORMAT PARQUET);",
            pq = pq_path.display()
        ))
        .expect("create parquet fixture");
    }
    let info = SourceInfo {
        id: SourceId::new("pqshop"),
        name: "PQ Shop".into(),
        kind: SourceKind::DuckDb,
        params: serde_json::json!({ "path": pq_path.display().to_string() }),
        created_at: String::new(),
    };
    let ds = connectors::connect(&info, "").await.unwrap();
    assert_eq!(ds.dialect(), connectors::Dialect::DuckDb);

    let cat = ds.catalog().await.unwrap();
    let alias = pq_path.file_stem().unwrap().to_str().unwrap().to_string();
    assert!(
        cat.tables.iter().any(|t| t.name == alias),
        "alias {alias} in catalog"
    );
    let t = cat.tables.iter().find(|t| t.name == alias).unwrap();
    assert!(t.columns.iter().any(|c| c.name == "amount_total"));

    let rows = ds
        .execute(
            &format!("SELECT COUNT(*) AS n FROM \"{alias}\""),
            &[],
            RowCap {
                limit: 10,
                timeout_secs: 5,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.rows[0][0], serde_json::json!(6));
}

#[cfg(feature = "duckdb")]
#[tokio::test]
async fn duckdb_database_file_and_lock_error() {
    let path = temp_path("duckdb");
    {
        let conn = duckdb::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (1),(2);")
            .unwrap();
        // hold the write lock open across the read-only attempt below
        let info = SourceInfo {
            id: SourceId::new("locked"),
            name: "locked".into(),
            kind: SourceKind::DuckDb,
            params: serde_json::json!({ "path": path.display().to_string() }),
            created_at: String::new(),
        };
        let result = connectors::connect(&info, "").await;
        // single-writer lock: read-only attach must fail with the actionable error
        let err = result
            .err()
            .expect("connect while writer holds the file must fail");
        assert!(
            err.message.contains("locked") || err.message.contains("close the other process"),
            "expected actionable lock error, got: {}",
            err.message
        );
    }
}

// ---- env-gated remote integration tests ----

fn env_dsn(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

#[tokio::test]
async fn postgres_integration() {
    let Some(dsn) = env_dsn("QUERORA_IT_PG") else {
        eprintln!("skipping: set QUERORA_IT_PG=postgres://user:pass@localhost:5432/db to run");
        return;
    };
    // seed
    let pool = sqlx::PgPool::connect(&dsn).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS orders, customers")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(SHOP_DDL).execute(&pool).await.unwrap();
    pool.close().await;

    let info = dsn_to_info(&dsn, SourceKind::Postgres, "it-pg");
    let ds = connectors::connect(&info, &dsn_password(&dsn))
        .await
        .unwrap();
    assert_eq!(ds.dialect(), connectors::Dialect::Pg);

    let cat = ds.catalog().await.unwrap();
    assert!(cat.tables.iter().any(|t| t.name == "orders"));
    let orders = cat.tables.iter().find(|t| t.name == "orders").unwrap();
    assert!(orders
        .foreign_keys
        .iter()
        .any(|f| f.column == "customer_id"));
    let prof = ds.profile("orders", "status", 1000).await.unwrap();
    assert_eq!(prof.distinct_count, Some(3));
    let rows = ds
        .execute(
            "SELECT COUNT(*) AS n FROM orders",
            &[],
            RowCap {
                limit: 5,
                timeout_secs: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.rows[0][0], serde_json::json!(6));
    let err = ds
        .execute("DELETE FROM orders", &[], RowCap::default())
        .await
        .unwrap_err();
    assert!(err.message.contains("read-only"));
}

#[tokio::test]
async fn mysql_integration() {
    let Some(dsn) = env_dsn("QUERORA_IT_MYSQL") else {
        eprintln!("skipping: set QUERORA_IT_MYSQL=mysql://user:pass@localhost:3306/db to run");
        return;
    };
    let info = dsn_to_info(&dsn, SourceKind::Mysql, "it-mysql");
    let ds = connectors::connect(&info, &dsn_password(&dsn))
        .await
        .unwrap();
    assert_eq!(ds.dialect(), connectors::Dialect::Mysql);

    // seed (mysql-safe DDL: separate FK clause; customers BEFORE orders)
    let pool = sqlx::MySqlPool::connect(&dsn).await.unwrap();
    sqlx::query("SET FOREIGN_KEY_CHECKS=0")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS orders, customers")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("SET FOREIGN_KEY_CHECKS=1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE customers (id INT PRIMARY KEY, name VARCHAR(255) NOT NULL, country VARCHAR(8), plan VARCHAR(16), created_at DATE)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT, status VARCHAR(16), amount_total DOUBLE, order_date DATE, \
         CONSTRAINT fk_o_c FOREIGN KEY (customer_id) REFERENCES customers(id))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO customers VALUES (1,'Ada','DK','pro','2026-01-05'),(2,'Linus','US','free','2026-01-20'),(3,'Grace','UK','enterprise','2026-02-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (id, cid, st, amt, d) in [
        (1, 1, "paid", 120.0, "2026-03-02"),
        (2, 1, "paid", 80.5, "2026-04-11"),
        (3, 2, "refunded", 60.0, "2026-04-19"),
        (4, 3, "paid", 300.0, "2026-05-01"),
        (5, 3, "pending", 45.0, "2026-06-15"),
        (6, 2, "paid", 99.9, "2026-06-30"),
    ] {
        sqlx::query("INSERT INTO orders VALUES (?,?,?,?,?)")
            .bind(id)
            .bind(cid)
            .bind(st)
            .bind(amt)
            .bind(d)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool.close().await;

    let cat = ds.catalog().await.unwrap();
    assert!(cat.tables.iter().any(|t| t.name == "orders"));
    let orders = cat.tables.iter().find(|t| t.name == "orders").unwrap();
    assert!(orders
        .foreign_keys
        .iter()
        .any(|f| f.column == "customer_id" && f.ref_table == "customers"));
    let rows = ds
        .execute(
            "SELECT COUNT(*) AS n FROM orders",
            &[],
            RowCap {
                limit: 5,
                timeout_secs: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.rows[0][0], serde_json::json!(6));
}

/// Parse `scheme://user:pass@host:port/db` into SourceInfo params.
fn dsn_to_info(dsn: &str, kind: SourceKind, id: &str) -> SourceInfo {
    let url = url::Url::parse(dsn).expect("valid DSN");
    SourceInfo {
        id: SourceId::new(id),
        name: id.into(),
        kind,
        params: serde_json::json!({
            "host": url.host_str().unwrap_or("localhost"),
            "port": url.port().unwrap_or(if kind == SourceKind::Mysql { 3306 } else { 5432 }),
            "database": url.path().trim_start_matches('/'),
            "user": url.username(),
        }),
        created_at: String::new(),
    }
}

fn dsn_password(dsn: &str) -> String {
    url::Url::parse(dsn)
        .ok()
        .and_then(|u| u.password().map(|p| p.to_string()))
        .unwrap_or_default()
}
