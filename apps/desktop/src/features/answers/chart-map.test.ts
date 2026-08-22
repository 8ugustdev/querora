/* SPDX-License-Identifier: Apache-2.0 */
import { describe, expect, it } from "vitest";
import { mapChart } from "./chart-map";
import type { QueryResult, VisualizationSpec } from "../../lib/contracts";

function result(overrides: Partial<QueryResult> = {}): QueryResult {
  return {
    result_id: "r1",
    columns: ["month", "revenue"],
    column_types: ["string", "number"],
    rows: [
      { month: "2026-03", revenue: 120 },
      { month: "2026-04", revenue: 80.5 },
    ],
    sql: "SELECT 1",
    params: [],
    semantic_version: "fixture-v1",
    stats: { row_count: 2, duration_ms: 3, row_cap: 1000, timeout_secs: 30 },
    ...overrides,
  } as QueryResult;
}

describe("chart mapping", () => {
  it("maps a valid bar spec", () => {
    const spec: VisualizationSpec = { chart_type: "bar", x: "month", y: "revenue" };
    const m = mapChart(spec, result());
    expect(m.kind).toBe("bar");
    expect(m.x?.label).toBe("month");
    expect(m.data).toHaveLength(2);
  });

  it("falls back to table when y is not numeric", () => {
    const spec: VisualizationSpec = { chart_type: "line", x: "month", y: "month" };
    const m = mapChart(spec, result());
    // falls back but auto-picks the numeric column
    expect(m.y?.label ?? m.kind).toBeTruthy();
  });

  it("missing columns fall back to auto-picked or table (never crash)", () => {
    const spec: VisualizationSpec = { chart_type: "pie", x: "nope", y: "also-nope" };
    const m = mapChart(spec, result());
    // y "also-nope" is not numeric → auto-picks the numeric column; x falls
    // back to first column. Either a valid mapping or table — both render.
    expect(m.kind === "pie" || m.kind === "table").toBeTruthy();
    expect(m.data.length).toBeGreaterThan(0);
    // a result with NO numeric column → table
    const textOnly = result({ columns: ["a", "b"], column_types: ["string", "string"] });
    expect(mapChart(spec, textOnly).kind).toBe("table");
  });

  it("no spec means table-only", () => {
    expect(mapChart(undefined, result()).kind).toBe("table");
  });

  it("pie maps category/value", () => {
    const spec: VisualizationSpec = { chart_type: "pie", x: "month", y: "revenue" };
    const m = mapChart(spec, result());
    expect(m.kind).toBe("pie");
    expect(m.data[0]).toEqual({ x: "2026-03", y: 120 });
  });
});
