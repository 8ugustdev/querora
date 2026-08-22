/* SPDX-License-Identifier: Apache-2.0 */
import type { QueryResult, VisualizationSpec } from "../../lib/contracts";

/** Mapped, render-ready chart description (Vega-Lite-shaped, minimal). */
export interface MappedChart {
  kind: "bar" | "line" | "area" | "pie" | "table";
  x: { field: string; label: string } | null;
  y: { field: string; label: string } | null;
  color: string | null;
  title: string;
  /** Rows reshaped to chart-friendly objects. */
  data: Array<Record<string, unknown>>;
}

/**
 * Map an agent VisualizationSpec onto a QueryResult's columns. Any invalid
 * mapping (missing columns, non-numeric y for charts) falls back to
 * `table` — an agent can never crash the answer.
 */
export function mapChart(spec: VisualizationSpec | undefined, result: QueryResult): MappedChart {
  const cols = new Set(result.columns);
  const numeric = (name: string) => {
    const i = result.columns.indexOf(name);
    return i >= 0 && (result.column_types[i] === "number" || result.column_types[i] === "integer");
  };
  const title = spec?.title ?? "Result";

  if (!spec || spec.chart_type === "table") return table(result, title);

  const x = spec.x && cols.has(spec.x) ? spec.x : result.columns[0];
  const y =
    spec.y && cols.has(spec.y) && numeric(spec.y)
      ? spec.y
      : result.columns.find((c) => numeric(c) && c !== x);
  if (!x || !y) return table(result, title);

  const data = result.rows.map((r) => ({ x: r[x], y: r[y], ...(spec.color && cols.has(spec.color) ? { c: r[spec.color] } : {}) }));

  switch (spec.chart_type) {
    case "bar":
    case "bar_horizontal":
      return { kind: "bar", x: { field: "x", label: x }, y: { field: "y", label: y }, color: spec.color ?? null, title, data };
    case "line":
      return { kind: "line", x: { field: "x", label: x }, y: { field: "y", label: y }, color: spec.color ?? null, title, data };
    case "area":
      return { kind: "area", x: { field: "x", label: x }, y: { field: "y", label: y }, color: spec.color ?? null, title, data };
    case "pie": {
      const cat = spec.x && cols.has(spec.x) ? spec.x : result.columns[0];
      const val = spec.y && numeric(spec.y) ? spec.y : result.columns.find((c) => numeric(c) && c !== cat);
      if (!cat || !val) return table(result, title);
      return {
        kind: "pie",
        x: { field: "x", label: cat },
        y: { field: "y", label: val },
        color: null,
        title,
        data: result.rows.map((r) => ({ x: r[cat], y: r[val] })),
      };
    }
    default:
      return table(result, title);
  }
}

function table(result: QueryResult, title: string): MappedChart {
  return { kind: "table", x: null, y: null, color: null, title, data: result.rows as Array<Record<string, unknown>> };
}
