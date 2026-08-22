// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Magento EAV unfolding: detects `eav_attribute` + `catalog_product_*`
//! schemas at draft time and extends the semantic graph with a flattened
//! `order_line` virtual entity (brand, cost, order time/status joined 1:1)
//! plus a filter-only `product_category` entity (semi-join — categories
//! are many-per-product, a LEFT JOIN would inflate every SUM).
//!
//! This is what makes questions like "orders from supplier HAY last year"
//! or "lamps sold last month vs margin last year" answerable: the data
//! lives in EAV tables the generic heuristic suggester cannot interpret.

use crate::connectors::{DataSource, RowCap};
use querora_contracts::semantic::{
    AggOp, ArithOp, Combination, Confidence, Dimension, Entity, JoinCardinality, JoinKind, Metric,
    MetricExpr, Relationship, SemanticDataType, SemanticGraph,
};
use querora_contracts::{ErrorCode, SourceId, ToolError};
use std::collections::{BTreeMap, BTreeSet};

/// Discovered EAV facts for one source.
#[derive(Debug, Clone, Default)]
pub struct EavInfo {
    /// Select-type product attributes (brand/manufacturer) — (attr_id, code).
    pub brand_attr: Option<(u64, String)>,
    /// Cost product attribute id.
    pub cost_attr: Option<u64>,
    /// Category-name attribute id (entity_type 3).
    pub category_name_attr: Option<u64>,
    /// Sales item table (M2 `sales_order_item` / M1 `sales_flat_order_item`).
    pub item_table: String,
    /// Sales order table.
    pub order_table: String,
}

/// Extension pieces merged into a heuristic draft.
#[derive(Debug, Default)]
pub struct EavExtension {
    /// Virtual entities to add (id → entity).
    pub entities: Vec<(String, Entity)>,
    /// Dimensions to add.
    pub dimensions: Vec<(String, Dimension)>,
    /// Metrics to add.
    pub metrics: Vec<(String, Metric)>,
    /// Relationships to add.
    pub relationships: Vec<Relationship>,
    /// Dimension id → sample values (FTS value-index).
    pub value_hints: BTreeMap<String, Vec<String>>,
}

/// Detect EAV structure on this source (cheap read-only SELECTs).
/// Returns `None` when the catalog isn't Magento-like.
pub async fn detect(
    ds: &dyn DataSource,
    catalog: &crate::connectors::types::DatabaseCatalog,
) -> Option<EavInfo> {
    let has = |name: &str| catalog.tables.iter().any(|t| t.name == name);
    if !has("eav_attribute") || !has("catalog_product_entity") {
        return None;
    }
    // item/order table detection (Magento 2 first, then Magento 1 names)
    let (item_table, order_table) = if has("sales_order_item") && has("sales_order") {
        ("sales_order_item".to_string(), "sales_order".to_string())
    } else if has("sales_flat_order_item") && has("sales_flat_order") {
        (
            "sales_flat_order_item".to_string(),
            "sales_flat_order".to_string(),
        )
    } else {
        return None;
    };

    let mut info = EavInfo {
        item_table,
        order_table,
        ..Default::default()
    };

    // product attributes (entity_type 4)
    if let Ok(rows) = ds
        .execute(
            "SELECT attribute_id, attribute_code, frontend_input FROM eav_attribute WHERE entity_type_id = 4 AND attribute_code IN ('brand', 'manufacturer', 'cost')",
            &[],
            RowCap { limit: 10, timeout_secs: 10 },
        )
        .await
    {
        for r in &rows.rows {
            let id = r.first().and_then(|v| v.as_u64());
            let code = r.get(1).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let input = r.get(2).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            match (code.as_str(), input.as_str()) {
                ("cost", _) => info.cost_attr = id,
                ("brand" | "manufacturer", "select") if info.brand_attr.is_none() => {
                    // prefer 'brand' if both appear (first wins)
                    info.brand_attr = id.map(|i| (i, code.clone()));
                }
                _ => {}
            }
        }
    }
    // category name attribute (entity_type 3)
    if let Ok(rows) = ds
        .execute(
            "SELECT attribute_id FROM eav_attribute WHERE entity_type_id = 3 AND attribute_code = 'name'",
            &[],
            RowCap { limit: 1, timeout_secs: 10 },
        )
        .await
    {
        info.category_name_attr = rows.rows.first().and_then(|r| r.first()).and_then(|v| v.as_u64());
    }
    Some(info)
}

/// Build the graph extension from discovered facts (pure — unit-testable).
pub fn build_extension(source: &SourceId, info: &EavInfo) -> EavExtension {
    let _ = source;
    let mut ext = EavExtension::default();
    let EavInfo {
        brand_attr,
        cost_attr,
        category_name_attr,
        item_table,
        order_table,
    } = info;

    // ---- order_line: flattened 1:1 view (no fan-out risk) ----
    let mut selects: Vec<String> = vec![
        "i.item_id".into(),
        "i.order_id".into(),
        "i.product_id".into(),
        "i.name AS product_name".into(),
        "COALESCE(i.base_row_total, i.row_total) AS row_total".into(),
        "i.qty_ordered".into(),
        "o.created_at AS order_date".into(),
        "o.status AS order_status".into(),
        "o.increment_id AS order_number".into(),
    ];
    if cost_attr.is_some() {
        selects.push("cst.value AS cost".into());
    }
    if brand_attr.is_some() {
        selects.push("br.value AS brand".into());
    }
    let mut joins: Vec<String> = vec![format!("JOIN {order_table} o ON o.entity_id = i.order_id")];
    if let Some(cost_id) = cost_attr {
        joins.push(format!(
            "LEFT JOIN (SELECT entity_id, value FROM catalog_product_entity_decimal WHERE attribute_id = {cost_id} AND store_id = 0) cst ON cst.entity_id = i.product_id"
        ));
    }
    if let Some((brand_id, _)) = brand_attr {
        joins.push(format!(
            "LEFT JOIN (SELECT ci.entity_id AS pid, ov.value AS value FROM catalog_product_entity_int ci \
             JOIN eav_attribute_option ao ON ao.attribute_id = {brand_id} AND ao.option_id = ci.value \
             JOIN eav_attribute_option_value ov ON ov.option_id = ao.option_id \
             WHERE ci.attribute_id = {brand_id} AND ci.store_id = 0) br ON br.pid = i.product_id"
        ));
    }
    let order_line_sql = format!(
        "SELECT {} FROM {item_table} i {}",
        selects.join(", "),
        joins.join(" ")
    );
    ext.entities.push((
        "order_line".into(),
        Entity {
            id: "order_line".into(),
            label: "Order Line (denormalized)".into(),
            table: "order_line".into(),
            description: Some("One row per order item with brand, cost, order date and status joined in (Magento EAV unfolded).".into()),
            definition_sql: Some(order_line_sql),
        },
    ));

    // dims on order_line
    for (id_suffix, label, col, dt, aliases) in [
        (
            "brand",
            "Brand",
            "brand",
            SemanticDataType::String,
            vec![
                "supplier".to_string(),
                "vendor".to_string(),
                "manufacturer".to_string(),
            ],
        ),
        (
            "product_name",
            "Product Name",
            "product_name",
            SemanticDataType::String,
            vec![],
        ),
        (
            "order_date",
            "Order Date",
            "order_date",
            SemanticDataType::Temporal,
            vec![],
        ),
        (
            "order_status",
            "Order Status",
            "order_status",
            SemanticDataType::String,
            vec![],
        ),
    ] {
        ext.dimensions.push((
            format!("order_line__{id_suffix}"),
            Dimension {
                id: format!("order_line__{id_suffix}"),
                label: label.into(),
                entity_id: "order_line".into(),
                column: col.into(),
                data_type: dt,
                aliases,
                description: None,
            },
        ));
    }

    // metrics on order_line
    let m = |id: &str, label: &str, expr: MetricExpr, aliases: Vec<String>| {
        (
            id.to_string(),
            Metric {
                id: id.into(),
                label: label.into(),
                entity_id: "order_line".into(),
                expr,
                aliases,
                description: None,
            },
        )
    };
    ext.metrics.push(m(
        "line_revenue",
        "Revenue (items)",
        MetricExpr {
            op: AggOp::Sum,
            column: Some("row_total".into()),
            human_formula: None,
            combination: None,
        },
        vec!["sales".into(), "turnover".into()],
    ));
    ext.metrics.push(m(
        "line_qty",
        "Units Sold",
        MetricExpr {
            op: AggOp::Sum,
            column: Some("qty_ordered".into()),
            human_formula: None,
            combination: None,
        },
        vec!["quantity".into(), "pieces sold".into()],
    ));
    ext.metrics.push(m(
        "orders_placed",
        "Orders Placed",
        MetricExpr {
            op: AggOp::CountDistinct,
            column: Some("order_id".into()),
            human_formula: Some("COUNT(DISTINCT order_id)".into()),
            combination: None,
        },
        vec!["number of orders".into(), "order count".into()],
    ));
    if cost_attr.is_some() {
        ext.metrics.push(m(
            "line_cost",
            "Cost (items)",
            MetricExpr {
                op: AggOp::Sum,
                column: Some("cost".into()),
                human_formula: None,
                combination: None,
            },
            vec![],
        ));
        ext.metrics.push(m(
            "margin_pct",
            "Margin %",
            MetricExpr {
                op: AggOp::Sum,
                column: None,
                human_formula: Some("(revenue - cost) / revenue".into()),
                // (revenue − cost) / revenue; Div NULLIF-guards denominator
                combination: Some(Combination {
                    left: Box::new(MetricExpr {
                        op: AggOp::Sum,
                        column: None,
                        human_formula: None,
                        combination: Some(Combination {
                            left: Box::new(MetricExpr {
                                op: AggOp::Sum,
                                column: Some("row_total".into()),
                                human_formula: None,
                                combination: None,
                            }),
                            op: ArithOp::Sub,
                            right: Box::new(MetricExpr {
                                op: AggOp::Sum,
                                column: Some("cost".into()),
                                human_formula: None,
                                combination: None,
                            }),
                        }),
                    }),
                    op: ArithOp::Div,
                    right: Box::new(MetricExpr {
                        op: AggOp::Sum,
                        column: Some("row_total".into()),
                        human_formula: None,
                        combination: None,
                    }),
                }),
            },
            vec!["margin".into(), "profit margin".into()],
        ));
    }

    // ---- product_category: filter-only semi-join entity ----
    if let Some(cat_attr) = category_name_attr {
        ext.entities.push((
            "product_category".into(),
            Entity {
                id: "product_category".into(),
                label: "Product Category".into(),
                table: "product_category".into(),
                description: Some("Product ↔ category mapping (a product can be in several categories; filter-only).".into()),
                definition_sql: Some(format!(
                    "SELECT cp.product_id, cv.value AS category_name FROM catalog_category_product cp \
                     JOIN catalog_category_entity ce ON ce.entity_id = cp.category_id AND ce.level > 1 \
                     JOIN (SELECT entity_id, value FROM catalog_category_entity_varchar WHERE attribute_id = {cat_attr} AND store_id = 0) cv ON cv.entity_id = cp.category_id"
                )),
            },
        ));
        ext.dimensions.push((
            "product_category__name".into(),
            Dimension {
                id: "product_category__name".into(),
                label: "Category".into(),
                entity_id: "product_category".into(),
                column: "category_name".into(),
                data_type: SemanticDataType::String,
                aliases: vec!["product type".into(), "product category".into()],
                description: None,
            },
        ));
        ext.relationships.push(Relationship {
            id: "order_line_to_category".into(),
            from_entity: "order_line".into(),
            from_column: "product_id".into(),
            to_entity: "product_category".into(),
            to_column: "product_id".into(),
            cardinality: JoinCardinality::ManyToOne,
            confidence: Confidence::Declared,
            join_kind: JoinKind::Semi,
        });
    }
    ext
}

/// Fetch sample values for value-aware search (top brands + categories).
pub async fn fetch_value_hints(
    ds: &dyn DataSource,
    info: &EavInfo,
) -> BTreeMap<String, Vec<String>> {
    let mut hints = BTreeMap::new();
    if let Some((brand_id, _)) = &info.brand_attr {
        let sql = format!(
            "SELECT ov.value, COUNT(*) c FROM catalog_product_entity_int ci \
             JOIN eav_attribute_option ao ON ao.attribute_id = {brand_id} AND ao.option_id = ci.value \
             JOIN eav_attribute_option_value ov ON ov.option_id = ao.option_id \
             WHERE ci.attribute_id = {brand_id} GROUP BY ov.value ORDER BY c DESC LIMIT 30"
        );
        if let Ok(rows) = ds
            .execute(
                &sql,
                &[],
                RowCap {
                    limit: 30,
                    timeout_secs: 15,
                },
            )
            .await
        {
            let vals: Vec<String> = rows
                .rows
                .iter()
                .filter_map(|r| r.first()?.as_str().map(str::to_string))
                .collect();
            if !vals.is_empty() {
                hints.insert("order_line__brand".to_string(), vals);
            }
        }
    }
    if let Some(cat_attr) = &info.category_name_attr {
        let sql = format!(
            "SELECT cv.value, COUNT(*) c FROM catalog_category_product cp \
             JOIN catalog_category_entity ce ON ce.entity_id = cp.category_id AND ce.level > 1 \
             JOIN (SELECT entity_id, value FROM catalog_category_entity_varchar WHERE attribute_id = {cat_attr} AND store_id = 0) cv ON cv.entity_id = cp.category_id \
             GROUP BY cv.value ORDER BY c DESC LIMIT 60"
        );
        if let Ok(rows) = ds
            .execute(
                &sql,
                &[],
                RowCap {
                    limit: 60,
                    timeout_secs: 15,
                },
            )
            .await
        {
            let vals: Vec<String> = rows
                .rows
                .iter()
                .filter_map(|r| r.first()?.as_str().map(str::to_string))
                .collect();
            if !vals.is_empty() {
                hints.insert("product_category__name".to_string(), vals);
            }
        }
    }
    hints
}

/// Merge an extension into a draft graph (ids overwrite heuristics).
pub fn merge(graph: &mut SemanticGraph, ext: &EavExtension) {
    for (id, e) in &ext.entities {
        graph.entities.insert(id.clone(), e.clone());
    }
    for (id, d) in &ext.dimensions {
        graph.dimensions.insert(id.clone(), d.clone());
    }
    for (id, m) in &ext.metrics {
        graph.metrics.insert(id.clone(), m.clone());
    }
    graph
        .relationships
        .extend(ext.relationships.iter().cloned());
}

/// Sanity-check extension consistency (unit tests use this).
pub fn extension_is_consistent(ext: &EavExtension) -> Result<(), ToolError> {
    let entity_ids: BTreeSet<&str> = ext.entities.iter().map(|(id, _)| id.as_str()).collect();
    for (_, d) in &ext.dimensions {
        if !entity_ids.contains(d.entity_id.as_str()) {
            return Err(ToolError::new(
                ErrorCode::Internal,
                format!("dim {} → unknown entity", d.id),
            ));
        }
    }
    for (_, m) in &ext.metrics {
        if !entity_ids.contains(m.entity_id.as_str()) {
            return Err(ToolError::new(
                ErrorCode::Internal,
                format!("metric {} → unknown entity", m.id),
            ));
        }
    }
    for r in &ext.relationships {
        if !entity_ids.contains(r.from_entity.as_str())
            || !entity_ids.contains(r.to_entity.as_str())
        {
            return Err(ToolError::new(
                ErrorCode::Internal,
                format!("rel {} → unknown endpoint", r.id),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> EavInfo {
        EavInfo {
            brand_attr: Some((153, "brand".into())),
            cost_attr: Some(81),
            category_name_attr: Some(45),
            item_table: "sales_order_item".into(),
            order_table: "sales_order".into(),
        }
    }

    #[test]
    fn extension_builds_consistently_with_all_pieces() {
        let ext = build_extension(&SourceId::new("m"), &info());
        assert!(extension_is_consistent(&ext).is_ok());
        let order_line = &ext
            .entities
            .iter()
            .find(|(id, _)| id == "order_line")
            .unwrap()
            .1;
        let sql = order_line.definition_sql.as_ref().unwrap();
        assert!(
            sql.contains("153") && sql.contains("81"),
            "brand+cost attrs embedded"
        );
        assert!(sql.contains("LEFT JOIN"), "1:1 left joins only");
        let semi = ext
            .relationships
            .iter()
            .find(|r| r.to_entity == "product_category")
            .unwrap();
        assert_eq!(semi.join_kind, JoinKind::Semi);
        // margin metric exists with combination
        let margin = &ext
            .metrics
            .iter()
            .find(|(id, _)| id == "margin_pct")
            .unwrap()
            .1;
        assert!(margin.expr.combination.is_some());
        // value hints attached to brand + category dims
        assert!(ext.value_hints.is_empty(), "hints fetched separately");
    }

    #[test]
    fn no_category_attr_still_builds() {
        let mut i = info();
        i.category_name_attr = None;
        let ext = build_extension(&SourceId::new("m"), &i);
        assert!(extension_is_consistent(&ext).is_ok());
        assert!(!ext.entities.iter().any(|(id, _)| id == "product_category"));
    }
}
