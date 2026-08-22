/* SPDX-License-Identifier: Apache-2.0 */
import { VegaEmbed } from "react-vega";
import type { MappedChart } from "./chart-map";

/** Lazy-loaded Vega renderer (bundle kept out of the main chunk). */
export default function VegaChart({ chart }: { chart: MappedChart }) {
  const spec = buildSpec(chart);
  // `as never`: react-vega's prop typing is stricter than its runtime needs
  const Embed = VegaEmbed as unknown as (p: { spec: unknown }) => JSX.Element;
  return <Embed spec={spec} />;
}

function buildSpec(chart: MappedChart): Record<string, unknown> {
  if (chart.kind === "pie") {
    return {
      data: { values: chart.data },
      mark: { type: "arc", innerRadius: 40 },
      encoding: {
        theta: { field: "y", type: "quantitative" },
        color: { field: "x", type: "nominal", title: chart.x?.label ?? null },
      },
      width: "container",
      height: 220,
    };
  }
  return {
    data: { values: chart.data },
    mark: chart.kind,
    encoding: {
      x: { field: "x", type: "nominal", title: chart.x?.label ?? null },
      y: { field: "y", type: "quantitative", title: chart.y?.label ?? null },
      ...(chart.color ? { color: { field: "c", type: "nominal" } } : {}),
    },
    width: "container",
    height: 220,
  };
}
