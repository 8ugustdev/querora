/* SPDX-License-Identifier: Apache-2.0 */
import { useState } from "react";
import type { QueryResult } from "../../lib/contracts";
import { exportCsv } from "../../lib/tauri-bridge";

/** Collapsible trust panel: IR → SQL → params → stats. */
export function TrustPanel({ result, irJson }: { result: QueryResult; irJson?: unknown }) {
  const [open, setOpen] = useState(false);
  const [exported, setExported] = useState<string | null>(null);
  return (
    <div className="mt-1 rounded-md border border-neutral-200 text-xs dark:border-neutral-800">
      <button
        className="flex w-full items-center justify-between px-2 py-1 text-left text-neutral-500 hover:bg-neutral-50 dark:hover:bg-neutral-800/40"
        onClick={() => setOpen(!open)}
      >
        <span>
          🔍 Trust panel · {String(result.stats.row_count)} rows · {String(result.stats.duration_ms)}ms · semantic {result.semantic_version}
        </span>
        <span>{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <div className="space-y-2 px-3 py-2">
          {irJson !== undefined && (
            <section>
              <h5 className="mb-1 font-semibold text-neutral-400">IR (agent-emitted)</h5>
              <pre className="overflow-auto rounded bg-neutral-100 p-2 text-[10px] leading-4 dark:bg-neutral-800">
                {JSON.stringify(irJson, null, 2)}
              </pre>
            </section>
          )}
          <section>
            <h5 className="mb-1 font-semibold text-neutral-400">SQL (Querora-compiled)</h5>
            <pre className="overflow-auto rounded bg-neutral-100 p-2 text-[10px] leading-4 dark:bg-neutral-800">
              {result.sql}
            </pre>
            {result.params.length > 0 && (
              <p className="mt-1 text-neutral-400">params: {JSON.stringify(result.params)}</p>
            )}
          </section>
          <section className="flex items-center gap-2">
            <button
              className="rounded border border-neutral-300 px-2 py-0.5 hover:bg-neutral-100 dark:border-neutral-700 dark:hover:bg-neutral-800"
              onClick={() => exportCsv(result.result_id).then(setExported).catch(() => setExported(null))}
            >
              Export CSV
            </button>
            {exported && <span className="truncate text-neutral-400">{exported}</span>}
          </section>
        </div>
      )}
    </div>
  );
}
