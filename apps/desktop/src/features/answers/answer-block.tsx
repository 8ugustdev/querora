/* SPDX-License-Identifier: Apache-2.0 */
import { Suspense, lazy, useMemo } from "react";
import type { QueryResult } from "../../lib/contracts";
import { mapChart, type MappedChart } from "./chart-map";

const VegaChart = lazy(() => import("./vega-chart"));

/** One rendered answer: chart (fallback table) + virtualized-ish table. */
export function AnswerBlock({ result, spec }: { result: QueryResult; spec?: Parameters<typeof mapChart>[0] }) {
  const chart = useMemo(() => mapChart(spec, result), [spec, result]);
  return (
    <section className="mt-2 rounded-lg border border-neutral-200 bg-white p-3 dark:border-neutral-800 dark:bg-neutral-900">
      <header className="mb-2 flex items-center justify-between">
        <h4 className="text-sm font-semibold">{chart.title}</h4>
        <span className="text-xs text-neutral-400">
          {String(result.stats.row_count)} rows · {String(result.stats.duration_ms)}ms · cap {String(result.stats.row_cap)}
        </span>
      </header>
      {chart.kind !== "table" && chart.x && chart.y ? (
        <ErrorBoundaryOr fallback={<ChartTable chart={chart} />}>
          <Suspense fallback={<ChartTable chart={chart} />}>
            <VegaChart chart={chart} />
          </Suspense>
        </ErrorBoundaryOr>
      ) : null}
      <details className="mt-2">
        <summary className="cursor-pointer text-xs text-neutral-500">Data table</summary>
        <ChartTable chart={chart} full />
      </details>
    </section>
  );
}

/** Simple table (virtualization: rows sliced; full data in <details>). */
export function ChartTable({ chart, full = false }: { chart: MappedChart; full?: boolean }) {
  const rows = full ? chart.data : chart.data.slice(0, 50);
  const cols = chart.kind === "table" ? Object.keys(rows[0] ?? {}) : ["x", "y", ...(chart.color ? ["c"] : [])];
  if (rows.length === 0) return <p className="text-xs text-neutral-400">No rows.</p>;
  return (
    <div className="max-h-72 overflow-auto rounded border border-neutral-200 dark:border-neutral-800">
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-neutral-100 text-left dark:bg-neutral-800">
          <tr>
            {cols.map((c) => (
              <th key={c} className="px-2 py-1 font-medium">
                {chart.kind === "table" ? c : c === "x" ? chart.x?.label : c === "y" ? chart.y?.label : "series"}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i} className="border-t border-neutral-100 dark:border-neutral-800/60">
              {cols.map((c) => (
                <td key={c} className="px-2 py-1 tabular-nums">
                  {String(r[c] ?? "")}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Minimal error boundary: agent/chart output can never crash the answer. */
import { Component, type ReactNode } from "react";
class ErrorBoundaryOr extends Component<{ children: ReactNode; fallback: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  render() {
    return this.state.failed ? this.props.fallback : this.props.children;
  }
}
