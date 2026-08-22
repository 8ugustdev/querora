/* SPDX-License-Identifier: Apache-2.0 */
import { useEffect, useReducer, useRef, useState } from "react";
import type { AgentEvent } from "../../lib/contracts";
import { chatSend, getAgentPrefs, getResult, onAgentEvent, sessionMessages } from "../../lib/tauri-bridge";
import { AnswerBlock } from "../answers/answer-block";
import { TrustPanel } from "../trust-panel/trust-panel";
import type { QueryResult, VisualizationSpec } from "../../lib/contracts";
import { chatReducer, initialChatState } from "./reducer";

const AGENTS = ["claude", "codex", "pi"] as const;

/** The main chat surface: stream + tool timeline + trust-panel hop-over. */
export function ChatView({ sessionId, onActivity }: { sessionId: string; onActivity?: () => void }) {
  const [state, dispatch] = useReducer(chatReducer, initialChatState);
  const [agent, setAgent] = useState<(typeof AGENTS)[number]>("pi");
  const [input, setInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pendingIds, setPendingIds] = useState<string[]>([]);
  void pendingIds;
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const off = onAgentEvent(sessionId, (ev: AgentEvent) => {
      dispatch({ type: "agent_event", ev });
      if (ev.type === "done") {
        dispatch({ type: "agent_end" });
        // attach any query results as charts + trust panels
        setPendingIds((ids) => {
          for (const id of ids) {
            getResult(id)
              .then((r) => {
                if (r) dispatch({ type: "attach_result", result: r as QueryResult, spec: inferSpec(r as QueryResult) });
              })
              .catch(() => {});
          }
          return [];
        });
      }
      if (ev.type === "tool_result" && (ev.value as { result_id?: string }).result_id) {
        setPendingIds((ids) => [...ids, (ev.value as { result_id: string }).result_id]);
      }
    });
    return off;
  }, [sessionId]);

  // default agent from prefs
  useEffect(() => {
    getAgentPrefs()
      .then((p) => setAgent((p.defaultAgent as (typeof AGENTS)[number]) ?? "pi"))
      .catch(() => {});
  }, []);

  // hydrate persisted history when (re)mounting on a session
  useEffect(() => {
    sessionMessages(sessionId)
      .then((rows) => {
        if (!rows.length) return;
        dispatch({
          type: "hydrate",
          messages: rows.map((r, i) => ({
            id: `h${i}`,
            role: r.role === "user" ? "user" : "agent",
            text: r.content?.text ?? "",
            toolTimeline: r.content?.tool_timeline ?? [],
            streaming: false,
            charts: [],
          })),
        });
      })
      .catch(() => {});
  }, [sessionId]);

  useEffect(() => {
    bottom.current?.scrollIntoView?.({ behavior: "smooth" });
  }, [state.messages]);

  async function send() {
    const text = input.trim();
    if (!text || state.busy) return;
    setInput("");
    setError(null);
    dispatch({ type: "user_send", text });
    try {
      const res = await chatSend(sessionId, agent, text);
      onActivity?.();
      if (!res.ok) setError("The agent did not produce an answer — see the timeline below.");
    } catch (e) {
      setError(String(e));
      dispatch({ type: "agent_end" });
    }
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex-1 space-y-3 overflow-y-auto px-6 py-4">
        {state.messages.map((m) => (
          <div key={m.id} className={m.role === "user" ? "flex justify-end" : "flex justify-start"}>
            <div
              className={
                m.role === "user"
                  ? "max-w-[70%] rounded-2xl bg-indigo-600 px-4 py-2 text-sm text-white"
                  : "max-w-[85%] rounded-2xl bg-neutral-100 px-4 py-2 text-sm dark:bg-neutral-900"
              }
            >
              {m.contextLost && (
                <p className="mb-2 rounded border border-amber-400 bg-amber-50 px-2 py-1 text-xs text-amber-700 dark:bg-amber-900/30 dark:text-amber-300">
                  ⚠ {m.contextLost}
                </p>
              )}
              {m.toolTimeline.length > 0 && (
                <ol className="mb-2 space-y-0.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                  {m.toolTimeline.map((t, i) => (
                    <li key={i}>
                      {t.kind === "call" ? `→ ${t.tool}` : `${t.ok ? "✓" : "✕"} ${t.tool} ${t.summary ?? ""}`}
                    </li>
                  ))}
                </ol>
              )}
              <p className="whitespace-pre-wrap">
                {m.text}
                {m.streaming && <span className="ml-0.5 animate-pulse">▍</span>}
              </p>
              {m.charts.map((c, i) => (
                <div key={i} className="mt-2">
                  <AnswerBlock result={c.result} spec={c.spec} />
                  <TrustPanel result={c.result} />
                </div>
              ))}
            </div>
          </div>
        ))}
        {error && <p className="text-center text-xs text-red-500">{error}</p>}
        <div ref={bottom} />
      </div>
      <div className="border-t border-neutral-200 px-6 py-3 dark:border-neutral-800">
        <div className="flex items-center gap-2">
          <select
            value={agent}
            onChange={(e) => setAgent(e.target.value as (typeof AGENTS)[number])}
            className="rounded-md border border-neutral-300 bg-transparent px-2 py-1.5 text-sm dark:border-neutral-700"
          >
            {AGENTS.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && send()}
            placeholder="Ask your data…"
            className="flex-1 rounded-md border border-neutral-300 bg-transparent px-3 py-1.5 text-sm outline-none focus:border-indigo-400 dark:border-neutral-700"
          />
          <button
            onClick={send}
            disabled={state.busy}
            className="rounded-md bg-indigo-600 px-4 py-1.5 text-sm font-medium text-white disabled:opacity-50"
          >
            {state.busy ? "…" : "Ask"}
          </button>
        </div>
      </div>
    </div>
  );

/** Heuristic chart: temporal-looking x + first numeric y → line, else bar. */
function inferSpec(r: QueryResult): VisualizationSpec {
  const temporal = (name: string) => /date|month|day|week|year|time|d0/i.test(name);
  const xIdx = r.columns.findIndex((c) => temporal(c));
  const x = xIdx >= 0 ? r.columns[xIdx] : r.columns[0];
  const yIdx = r.columns.findIndex(
    (_c, i) => i !== xIdx && (r.column_types[i] === "number" || r.column_types[i] === "integer"),
  );
  const y = yIdx >= 0 ? r.columns[yIdx] : undefined;
  return {
    chart_type: xIdx >= 0 && y ? "line" : y ? "bar" : "table",
    x,
    y,
    title: y ? `${y} by ${x}` : "Result",
  };
}
}
