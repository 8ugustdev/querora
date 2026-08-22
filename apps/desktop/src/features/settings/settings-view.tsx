/* SPDX-License-Identifier: Apache-2.0 */
import { useEffect, useState } from "react";
import type { AgentStatus, SourceInfo, SourceKind } from "../../lib/contracts";
import { addSource, dualmodeConnections, dualmodeDisable, dualmodeEnable, getAgentPrefs, listSources, probeAgents, removeSource, setAgentPrefs, testSource, type AgentPrefs } from "../../lib/tauri-bridge";

/** Settings: agent status cards + source CRUD + BYOK placeholder. */
export function SettingsView() {
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [sources, setSources] = useState<SourceInfo[]>([]);
  const [form, setForm] = useState({ name: "Shop", kind: "sqlite" as SourceKind, path: "", host: "", port: "", database: "", user: "", secret: "" });
  const [test, setTest] = useState<string | null>(null);

  useEffect(() => {
    probeAgents().then(setAgents).catch(() => setAgents([]));
    listSources().then(setSources).catch(() => setSources([]));
  }, []);

  const info = (): SourceInfo => {
    const id = form.name.toLowerCase().replace(/[^a-z0-9-]+/g, "-");
    const params =
      form.kind === "sqlite" || form.kind === "duck_db"
        ? { path: form.path }
        : { host: form.host, port: Number(form.port || (form.kind === "mysql" ? 3306 : 5432)), database: form.database, user: form.user };
    return { id, name: form.name, kind: form.kind, params, created_at: new Date().toISOString() };
  };

  return (
    <div className="mx-auto max-w-2xl space-y-8 px-6 py-6">
      <AgentPrefsSection />

      <section>
        <h3 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-400">Agents</h3>
        <div className="grid grid-cols-3 gap-3">
          {agents.map((a) => (
            <div key={a.agent} className="rounded-lg border border-neutral-200 p-3 dark:border-neutral-800">
              <div className="flex items-center gap-2">
                <span className={`h-2 w-2 rounded-full ${a.installed ? "bg-emerald-500" : "bg-neutral-300 dark:bg-neutral-700"}`} />
                <span className="font-medium">{a.agent}</span>
              </div>
              <p className="mt-1 text-xs text-neutral-400">{a.version ?? a.note ?? "not installed"}</p>
            </div>
          ))}
          {agents.length === 0 && <p className="text-xs text-neutral-400">Probing…</p>}
        </div>
        <p className="mt-2 text-xs text-neutral-400">
          BYOK (bring your own key): Settings → Agents → pi + custom provider — lands with the BYOK
          sidecar config (v1 feature gate stub).
        </p>
      </section>

      <section>
        <h3 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-400">Terminal access (dual mode)</h3>
        <TerminalAccess />
      </section>

      <section>
        <h3 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-400">Sources</h3>
        <ul className="mb-4 space-y-1">
          {sources.map((s) => (
            <li key={s.id} className="flex items-center justify-between rounded border border-neutral-200 px-3 py-2 text-sm dark:border-neutral-800">
              <span>
                {s.name} <span className="text-xs text-neutral-400">({s.kind} · {s.id})</span>
              </span>
              <button className="text-xs text-red-500" onClick={() => removeSource(s.id).then(() => listSources().then(setSources))}>
                remove
              </button>
            </li>
          ))}
          {sources.length === 0 && <li className="text-xs text-neutral-400">No sources yet — add one below.</li>}
        </ul>

        <div className="grid grid-cols-2 gap-2 rounded-lg border border-neutral-200 p-3 text-sm dark:border-neutral-800">
          <label className="flex items-center gap-2">
            Name
            <input className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
          </label>
          <label className="flex items-center gap-2">
            Kind
            <select className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value as SourceKind })}>
              <option value="sqlite">sqlite</option>
              <option value="duck_db">duckdb / parquet / csv</option>
              <option value="postgres">postgres</option>
              <option value="mysql">mysql</option>
            </select>
          </label>
          {(form.kind === "sqlite" || form.kind === "duck_db") && (
            <label className="col-span-2 flex items-center gap-2">
              Path
              <input className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" placeholder="/path/to/shop.db" value={form.path} onChange={(e) => setForm({ ...form, path: e.target.value })} />
            </label>
          )}
          {form.kind !== "sqlite" && form.kind !== "duck_db" && (
            <>
              <label className="flex items-center gap-2">
                Host
                <input className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.host} onChange={(e) => setForm({ ...form, host: e.target.value })} />
              </label>
              <label className="flex items-center gap-2">
                Port
                <input className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.port} onChange={(e) => setForm({ ...form, port: e.target.value })} />
              </label>
              <label className="flex items-center gap-2">
                Database
                <input className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.database} onChange={(e) => setForm({ ...form, database: e.target.value })} />
              </label>
              <label className="flex items-center gap-2">
                User
                <input className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.user} onChange={(e) => setForm({ ...form, user: e.target.value })} />
              </label>
              <label className="col-span-2 flex items-center gap-2">
                Password (Keychain)
                <input type="password" className="flex-1 rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700" value={form.secret} onChange={(e) => setForm({ ...form, secret: e.target.value })} />
              </label>
            </>
          )}
          <div className="col-span-2 flex items-center gap-2">
            <button
              className="rounded border border-neutral-300 px-3 py-1 text-xs dark:border-neutral-700"
              onClick={async () => {
                setTest("testing…");
                try {
                  const r = await testSource(info(), form.secret || undefined);
                  setTest(`✓ ${r.tables} tables (${r.dialect})`);
                } catch (e) {
                  setTest(`✕ ${String(e)}`);
                }
              }}
            >
              Test connection
            </button>
            <button
              className="rounded bg-indigo-600 px-3 py-1 text-xs text-white"
              onClick={async () => {
                try {
                  await addSource(info(), form.secret || undefined);
                  setTest("added ✓");
                  setSources(await listSources());
                } catch (e) {
                  setTest(`✕ ${String(e)}`);
                }
              }}
            >
              Add source
            </button>
            {test && <span className="text-xs text-neutral-400">{test}</span>}
          </div>
        </div>
      </section>
    </div>
  );

function TerminalAccess() {
  const [info, setInfo] = useState<{ token_file: string; claude: string; codex: string } | null>(null);
  const [conns, setConns] = useState<Array<{ ts: string; tool: string; summary: string }>>([]);
  const refresh = () => dualmodeConnections().then(setConns).catch(() => {});
  useEffect(() => { refresh(); }, []);
  return (
    <div className="space-y-2 text-sm">
      <div className="flex items-center gap-2">
        <button
          className="rounded-md bg-indigo-600 px-3 py-1 text-xs text-white"
          onClick={async () => {
            setInfo(await dualmodeEnable());
            refresh();
          }}
        >
          Enable (rotate token)
        </button>
        <button
          className="rounded-md border border-neutral-300 px-3 py-1 text-xs dark:border-neutral-700"
          onClick={async () => {
            setInfo(null);
            await dualmodeDisable();
            refresh();
          }}
        >
          Disable
        </button>
      </div>
      {info && (
        <div className="space-y-1 rounded-lg border border-neutral-200 p-3 text-xs dark:border-neutral-800">
          <p className="text-neutral-400">Token file (0600): {info.token_file}</p>
          <p className="font-semibold">Claude Code</p>
          <pre className="overflow-auto rounded bg-neutral-100 p-2 dark:bg-neutral-800">{info.claude}</pre>
          <p className="font-semibold">Codex (~/.codex/config.toml — merge with consent)</p>
          <pre className="overflow-auto rounded bg-neutral-100 p-2 dark:bg-neutral-800">{info.codex}</pre>
        </div>
      )}
      {conns.length > 0 && (
        <ul className="max-h-32 space-y-0.5 overflow-y-auto rounded border border-neutral-200 p-2 text-xs text-neutral-500 dark:border-neutral-800">
          {conns.map((c, i) => (
            <li key={i}>
              {c.ts.slice(0, 19)} · {c.tool} · {c.summary}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const EFFORTS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const PI_MODELS = [
  "zai/glm-5.3",
  "zai/glm-5.2",
  "zai/glm-5.2-highspeed",
  "zai/glm-5-turbo",
  "zai/glm-4.7",
];

function AgentPrefsSection() {
  const [prefs, setPrefs] = useState<AgentPrefs | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => {
    getAgentPrefs().then(setPrefs).catch(() => setPrefs(null));
  }, []);
  if (!prefs) return null;
  const upd = (patch: Partial<AgentPrefs>) => {
    const next = { ...prefs, ...patch };
    setPrefs(next);
    setSaved(false);
    setAgentPrefs(next).then(() => setSaved(true)).catch(() => setSaved(false));
  };
  return (
    <section>
      <h3 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-400">Defaults</h3>
      <div className="grid grid-cols-3 gap-3 rounded-lg border border-neutral-200 p-3 text-sm dark:border-neutral-800">
        <label className="flex flex-col gap-1">
          <span className="text-xs text-neutral-400">Default agent</span>
          <select
            value={prefs.defaultAgent}
            onChange={(e) => upd({ defaultAgent: e.target.value })}
            className="rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700"
          >
            <option value="pi">pi</option>
            <option value="claude">claude</option>
            <option value="codex">codex</option>
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-xs text-neutral-400">Pi model</span>
          <select
            value={PI_MODELS.includes(prefs.piModel) ? prefs.piModel : "custom"}
            onChange={(e) => e.target.value !== "custom" && upd({ piModel: e.target.value })}
            className="rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700"
          >
            {PI_MODELS.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
            {!PI_MODELS.includes(prefs.piModel) && <option value="custom">{prefs.piModel} (custom)</option>}
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-xs text-neutral-400">Pi effort</span>
          <select
            value={prefs.piEffort}
            onChange={(e) => upd({ piEffort: e.target.value })}
            className="rounded border border-neutral-300 bg-transparent px-2 py-1 dark:border-neutral-700"
          >
            {EFFORTS.map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
        </label>
        <p className="col-span-3 text-xs text-neutral-400">
          New chats start with the default agent{prefs.defaultAgent === "pi" ? ` — pi runs ${prefs.piModel} @ ${prefs.piEffort} effort via the local sidecar` : ""}.
          {saved && <span className="ml-1 text-emerald-500">saved ✓</span>}
        </p>
      </div>
    </section>
  );
}
}
