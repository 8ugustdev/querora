/* SPDX-License-Identifier: Apache-2.0 */
import { useEffect, useState } from "react";
import type { SemanticGraph } from "../../lib/contracts";
import { draftSemantics, enrichSemantics, introspect, listSources, publishSemantics } from "../../lib/tauri-bridge";

interface DriftEntry {
  type: string;
  value: { table?: string; column?: string };
}

/** Semantic layer studio + schema explorer (draft → enrich → publish). */
export function SemanticView() {
  const [sources, setSources] = useState<Array<{ id: string; name: string; kind: string }>>([]);
  const [source, setSource] = useState("");
  const [draft, setDraft] = useState<SemanticGraph | null>(null);
  const [flags, setFlags] = useState<{ unjoined_tables: string[]; candidate_relationships: string[] }>({ unjoined_tables: [], candidate_relationships: [] });
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [enrichLog, setEnrichLog] = useState<string[]>([]);
  const [elapsed, setElapsed] = useState(0);
  const [drift, setDrift] = useState<DriftEntry[] | null>(null);
  const [tables, setTables] = useState<Array<{ name: string; is_view: boolean; columns: Array<{ name: string; data_type: string; nullable: boolean; primary_key: boolean }> }>>([]);

  useEffect(() => {
    let t: ReturnType<typeof setInterval> | undefined;
    let off: (() => void) | undefined;
    import("@tauri-apps/api/event").then(({ listen }) => {
      listen("agent-event://semantic-enrich", (e) => {
        const ev = e.payload as { type?: string; text?: string };
        if (ev?.type === "status" && ev.text) setEnrichLog((l) => [...l.slice(-3), ev.text!]);
        if (ev?.type === "done") {
          if (t) clearInterval(t);
          setElapsed((s) => (s > 0 ? s : 0));
        }
      }).then((u) => (off = u));
    });
    return () => {
      off?.();
      if (t) clearInterval(t);
    };
  }, []);

  useEffect(() => {
    listSources().then((s) => {
      setSources(s as never);
      if (s[0]) setSource(s[0].id);
    });
  }, []);

  async function run<T>(label: string, fn: () => Promise<T>): Promise<T | null> {
    setBusy(true);
    setStatus(`${label}…`);
    try {
      return await fn();
    } catch (e) {
      setStatus(`✕ ${String(e)}`);
      return null;
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="mx-auto max-w-3xl space-y-5 px-6 py-6">
      <section className="flex items-center gap-2">
        <select value={source} onChange={(e) => setSource(e.target.value)} className="rounded-md border border-neutral-300 bg-transparent px-2 py-1.5 text-sm dark:border-neutral-700">
          {sources.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name} ({s.id})
            </option>
          ))}
          {sources.length === 0 && <option value="">add a source in Settings first</option>}
        </select>
        <button
          disabled={!source || busy}
          className="rounded-md bg-indigo-600 px-3 py-1.5 text-sm text-white disabled:opacity-50"
          onClick={() =>
            run("Drafting", async () => {
              const r = await draftSemantics(source);
              setDraft(r.graph);
              setFlags({ unjoined_tables: r.unjoined_tables, candidate_relationships: r.candidate_relationships });
              setStatus(`✓ draft: ${Object.keys(r.graph.metrics).length} metrics, ${Object.keys(r.graph.dimensions).length} dimensions, ${r.graph.relationships.length} relationships`);
            })
          }
        >
          1 · Draft model
        </button>
        <button
          disabled={!draft || busy}
          className="rounded-md border border-neutral-300 px-3 py-1.5 text-sm disabled:opacity-50 dark:border-neutral-700"
          onClick={async () => {
            setEnrichLog([]);
            setElapsed(1);
            const timer = setInterval(() => setElapsed((s) => s + 1), 1000);
            try {
              await run("Enriching (agent) — typically 30–90s", async () => {
                const r = await enrichSemantics(source);
                setDraft(r.graph);
                setStatus(
                  r.status === "enriched"
                    ? "✓ agent enriched the draft — review before publishing"
                    : `↩ fell back to heuristic (${r.reason})`,
                );
              });
            } finally {
              clearInterval(timer);
            }
          }}
        >
          2 · Enrich (AI)
        </button>
        <button
          disabled={!draft || busy}
          className="rounded-md bg-emerald-600 px-3 py-1.5 text-sm text-white disabled:opacity-50"
          onClick={() =>
            run("Publishing", async () => {
              const v = await publishSemantics(source);
              setStatus(`✓ published ${v} (immutable) — the compiler now accepts only this version`);
            })
          }
        >
          3 · Publish
        </button>
      </section>
      {status && <p className="text-xs text-neutral-500">{status}</p>}
      {busy && elapsed > 0 && (
        <div className="rounded-md border border-indigo-200 bg-indigo-50 px-3 py-2 text-xs text-indigo-700 dark:border-indigo-800 dark:bg-indigo-900/20 dark:text-indigo-300">
          <span className="mr-2 inline-block h-2 w-2 animate-pulse rounded-full bg-indigo-500" />
          working… {elapsed}s elapsed{enrichLog.length > 0 && <span className="block text-indigo-500/80">{enrichLog.at(-1)}</span>}
        </div>
      )}

      {flags.unjoined_tables.length > 0 && (
        <section className="max-h-40 overflow-y-auto rounded-lg border border-amber-300 bg-amber-50 p-3 text-xs text-amber-800 dark:border-amber-700 dark:bg-amber-900/20 dark:text-amber-300">
          <strong>Unjoined tables</strong> (no declared FK or naming-convention match):{" "}
          {flags.unjoined_tables.join(", ")} — add relationships manually after review.
          {flags.candidate_relationships.length > 0 && (
            <>
              <br />
              <strong>Candidate joins</strong> (naming convention, lower confidence):{" "}
              {flags.candidate_relationships.join("; ")}
            </>
          )}
        </section>
      )}

      {draft && (
        <section className="grid grid-cols-3 gap-3">
          <GraphCard title="Entities" items={Object.values(draft.entities).map((e) => `${e?.label} (${e?.table})`)} />
          <GraphCard title="Metrics" items={Object.values(draft.metrics).map((m) => `${m?.label}${m?.aliases?.length ? ` ⟂ ${m.aliases.join(",")}` : ""}`)} />
          <GraphCard title="Dimensions" items={Object.values(draft.dimensions).map((d) => `${d?.label} [${d?.data_type}]`)} />
        </section>
      )}

      <section>
        <div className="mb-2 flex items-center justify-between">
          <h3 className="text-sm font-semibold uppercase tracking-wide text-neutral-400">Schema explorer & drift</h3>
          <button
            disabled={!source || busy}
            className="rounded border border-neutral-300 px-2 py-1 text-xs dark:border-neutral-700"
            onClick={() =>
              run("Introspecting", async () => {
                const r = await introspect(source);
                setTables(r.catalog.tables);
                setDrift(r.drift?.entries ?? null);
                setStatus(r.drift && r.drift.entries.length ? `⚠ drift: ${r.drift.entries.length} change(s)` : "✓ schema in sync");
              })
            }
          >
            Re-introspect
          </button>
        </div>
        {drift && drift.length > 0 && (
          <ul className="mb-2 space-y-0.5 rounded border border-amber-300 bg-amber-50 p-2 text-xs dark:border-amber-700 dark:bg-amber-900/20">
            {drift.map((d, i) => (
              <li key={i}>
                {d.type}: {d.value.table}
                {d.value.column ? `.${d.value.column}` : ""}
              </li>
            ))}
          </ul>
        )}
        <div className="space-y-1">
          {tables.map((t) => (
            <details key={t.name} className="rounded border border-neutral-200 px-2 py-1 text-xs dark:border-neutral-800">
              <summary className="cursor-pointer">
                {t.name} {t.is_view && <span className="text-neutral-400">(view)</span>}
              </summary>
              <ul className="mt-1 pl-4 text-neutral-500">
                {t.columns.map((c) => (
                  <li key={c.name}>
                    {c.name}: {c.data_type}
                    {c.primary_key && " 🔑"} {c.nullable && "(nullable)"}
                  </li>
                ))}
              </ul>
            </details>
          ))}
          {tables.length === 0 && <p className="text-xs text-neutral-400">Run re-introspect to browse tables.</p>}
        </div>
      </section>
    </div>
  );
}

function GraphCard({ title, items }: { title: string; items: string[] }) {
  return (
    <div className="rounded-lg border border-neutral-200 p-3 dark:border-neutral-800">
      <h4 className="mb-1 text-xs font-semibold uppercase tracking-wide text-neutral-400">{title}</h4>
      <ul className="max-h-56 space-y-0.5 overflow-y-auto text-xs">
        {items.slice(0, 100).map((i) => (
          <li key={i} className="truncate">
            {i}
          </li>
        ))}
        {items.length > 100 && <li className="text-neutral-400">+{items.length - 100} more…</li>}
      </ul>
    </div>
  );
}
