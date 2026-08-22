/* SPDX-License-Identifier: Apache-2.0 */
import { useEffect, useState } from "react";
import { ChatView } from "./features/chat/chat-view";
import { SemanticView } from "./features/semantic/semantic-view";
import { SettingsView } from "./features/settings/settings-view";
import { listSessions, ping, type SessionDto } from "./lib/tauri-bridge";

type Tab = "chat" | "schema" | "settings";

export default function App() {
  const [tab, setTab] = useState<Tab>("chat");
  const [bridge, setBridge] = useState<"ok" | "frontend-only" | "checking">("checking");
  const [sessions, setSessions] = useState<SessionDto[]>([]);
  const [active, setActive] = useState<string>(() => `s-${Date.now().toString(36)}`);

  useEffect(() => {
    let cancelled = false;
    ping()
      .then(() => !cancelled && setBridge("ok"))
      .catch(() => !cancelled && setBridge("frontend-only"));
    listSessions()
      .then((s) => !cancelled && setSessions(s))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="flex h-full bg-neutral-50 text-neutral-900 dark:bg-neutral-950 dark:text-neutral-100">
      <aside className="flex w-56 shrink-0 flex-col border-r border-neutral-200 bg-neutral-100/60 dark:border-neutral-800 dark:bg-neutral-900/40">
        <div className="flex items-center gap-2 px-4 py-4">
          <img src="/querora.svg" alt="" className="h-7 w-7" />
          <span className="text-lg font-semibold tracking-tight">Querora</span>
        </div>
        <nav className="mt-2 flex flex-col gap-1 px-2">
          <TabBtn active={tab === "chat"} onClick={() => setTab("chat")}>
            Chat
          </TabBtn>
          <TabBtn active={tab === "schema"} onClick={() => setTab("schema")}>
            Schema
          </TabBtn>
          <TabBtn active={tab === "settings"} onClick={() => setTab("settings")}>
            Settings
          </TabBtn>
        </nav>
        {tab === "chat" && (
          <div className="mt-6 flex min-h-0 flex-1 flex-col px-2">
            <p className="px-2 pb-1 text-[11px] uppercase tracking-wide text-neutral-400">Sessions</p>
            <div className="min-h-0 flex-1 overflow-y-auto">
              {sessions.map((s) => (
                <button
                  key={s.id}
                  onClick={() => setActive(s.id)}
                  className={`mb-0.5 block w-full truncate rounded-md px-3 py-1.5 text-left text-sm ${
                    s.id === active ? "bg-indigo-600/10 text-indigo-600 dark:text-indigo-400" : "text-neutral-500 dark:text-neutral-400"
                  }`}
                >
                  {s.title || s.id} <span className="text-[10px] text-neutral-400">· {s.agent}</span>
                </button>
              ))}
            </div>
            <button
              className="mx-2 mb-3 rounded-md border border-neutral-300 px-2 py-1 text-xs text-neutral-500 dark:border-neutral-700"
              onClick={() => {
                const id = `s-${Date.now().toString(36)}`;
                setActive(id);
                setSessions([{ id, agent: "claude", title: "", createdAt: "", updatedAt: "" }, ...sessions]);
              }}
            >
              + New chat
            </button>
          </div>
        )}
        <div className="mt-auto px-4 py-3 text-xs text-neutral-400">
          {bridge === "checking" && "…"}
          {bridge === "ok" && "core bridge: connected"}
          {bridge === "frontend-only" && "core bridge: unavailable (browser mode)"}
        </div>
      </aside>
      <main className="flex-1 overflow-hidden">
        {tab === "chat" ? <ChatView sessionId={active} onActivity={() => listSessions().then(setSessions).catch(() => {})} /> : tab === "schema" ? <SemanticView /> : <SettingsView />}
      </main>
    </div>
  );
}

function TabBtn({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={`rounded-md px-3 py-1.5 text-left text-sm ${
        active
          ? "bg-white font-medium shadow-sm dark:bg-neutral-800"
          : "text-neutral-500 hover:bg-white/60 dark:text-neutral-400 dark:hover:bg-neutral-800/60"
      }`}
    >
      {children}
    </button>
  );
}
