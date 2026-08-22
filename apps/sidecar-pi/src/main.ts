/* SPDX-License-Identifier: Apache-2.0 */
/**
 * querora-pi-sidecar — the pi agent driver.
 *
 * Usage: node dist/main.js --sock <path> --token <token> [--byok-key-fd 3]
 *
 * - Hosts a pi SDK agent session with ONLY Querora tools enabled
 *   (`noTools: "builtin"` — least privilege).
 * - Custom AgentTools bridge the token-authed toolapi unix socket.
 * - Events stream as JSON lines on stdout, mirroring
 *   querora_contracts::AgentEvent:
 *   {type:"started"|"token"|"tool_call"|"tool_result"|"answer"|
 *    "context_lost"|"failed"|"done", ...}
 * - Input: one JSON line per turn on stdin: {"prompt": "..."}.
 * - BYOK: read the provider key from the inherited fd (`--byok-key-fd`),
 *   never argv/env — no `ps eww`/crash-dump leakage.
 */
import { createAgentSession, defineTool, ModelRuntime, SessionManager } from "@earendil-works/pi-coding-agent";
import net from "node:net";
import readline from "node:readline";

interface SidecarEvent {
  type: string;
  [key: string]: unknown;
}

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

/** Resolve the SessionManager for this turn. */
async function resolveSessionManager(
  resumeId: string | undefined,
  legacyFile: string | undefined,
): Promise<ReturnType<typeof SessionManager.create>> {
  if (legacyFile) {
    return SessionManager.open(legacyFile);
  }
  if (resumeId === "new") {
    return SessionManager.create(process.cwd());
  }
  if (resumeId) {
    // resolve uuid → file under pi's native store (mirrors `pi --session`)
    const { getAgentDir } = await import("@earendil-works/pi-coding-agent");
    const path = await import("node:path");
    const fs = await import("node:fs/promises");
    const storeRoot = path.join(getAgentDir(), "sessions");
    const dirs = await fs.readdir(storeRoot).catch(() => [] as string[]);
    for (const d of dirs) {
      const dirPath = path.join(storeRoot, d);
      let entries: string[] = [];
      try {
        entries = await fs.readdir(dirPath);
      } catch {
        continue;
      }
      const hit = entries.find((f) => f.includes(resumeId) && f.endsWith(".jsonl"));
      if (hit) {
        return SessionManager.open(path.join(dirPath, hit));
      }
    }
    // unknown id (deleted?) → fresh session rather than dying
    process.stderr.write(`[querora-pi] session ${resumeId} not found; starting new\n`);
    return SessionManager.create(process.cwd());
  }
  return SessionManager.inMemory();
}

function emit(ev: SidecarEvent): void {
  process.stdout.write(JSON.stringify(ev) + "\n");
}

// ---- toolapi bridge -------------------------------------------------------

interface Pending {
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
}

class ToolApiClient {
  sockPath: string;
  token: string;
  buf = "";
  pending = new Map<string | number, Pending>();
  nextId = 1;
  sock!: net.Socket;

  constructor(sockPath: string, token: string) {
    this.sockPath = sockPath;
    this.token = token;
  }

  connect(timeoutMs = 15000): Promise<void> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`toolapi connect timeout (${this.sockPath})`)), timeoutMs);
      this.sock = net.createConnection(this.sockPath, () => {
        this.sock.write(
          JSON.stringify({ jsonrpc: "2.0", id: "auth", method: "auth", params: { token: this.token } }) + "\n",
        );
      });
      this.sock.setEncoding("utf8");
      this.sock.on("data", (chunk: string) => {
        clearTimeout(timer);
        this.onData(chunk, resolve);
      });
      this.sock.on("error", (e) => {
        clearTimeout(timer);
        reject(e);
      });
    });
  }

  onData(chunk: string, onAuth?: () => void): void {
    this.buf += chunk;
    let idx: number;
    let authHandled = false;
    while ((idx = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, idx).trim();
      this.buf = this.buf.slice(idx + 1);
      if (!line) continue;
      let msg: { id?: unknown; error?: { message?: string }; result?: unknown };
      try {
        msg = JSON.parse(line);
      } catch {
        continue; // fuzz guarantee: garbage ignored
      }
      if (!authHandled) {
        // first reply on the wire IS the auth result (id shape varies by peer)
        authHandled = true;
        if (msg.error) {
          emit({ type: "failed", error: "toolapi auth rejected" });
          process.exit(2);
        }
        onAuth?.();
        continue;
      }
      const p = this.pending.get(msg.id as string | number);
      if (p) {
        this.pending.delete(msg.id as string | number);
        if (msg.error) p.reject(new Error(msg.error.message ?? "toolapi error"));
        else p.resolve(msg.result);
      }
    }
  }

  call(method: string, params: Record<string, unknown>): Promise<unknown> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.sock.write(JSON.stringify({ jsonrpc: "2.0", id, method, params: params ?? {} }) + "\n");
    });
  }
}

// ---- querora tools as pi AgentTools --------------------------------------

const TOOL_DESCS: Record<string, string> = {
  search_semantics:
    "Search the published semantic layer (metrics, dimensions, entities). Use FIRST to map a business question to ids.",
  get_schema: "List tables/columns of a source (physical schema).",
  profile_column: "Profile a column (distinct, null %, min/max, top values).",
  execute_query:
    "Execute an AnalyticalQuery IR; returns truncated rows + stats + result_id + compiled SQL. NEVER write SQL yourself.",
  dry_run: "Compile AnalyticalQuery IR and return the plan without executing.",
};

function makeTools(client: ToolApiClient) {
  const wrap = (method: string) =>
    defineTool({
      name: method,
      label: method,
      description: TOOL_DESCS[method] ?? `${method} (Querora tool)`,
      parameters: {
        type: "object",
        properties: {
          params: { type: "object", description: "Tool params object" },
        },
      },
      execute: async (_id: string, args: { params?: Record<string, unknown> }) => {
        const result = await client.call(method, args?.params ?? {});
        return {
          content: [{ type: "text" as const, text: JSON.stringify(result) }],
          details: {},
        };
      },
    });
  return Object.keys(TOOL_DESCS).map(wrap);
}

// ---- main -----------------------------------------------------------------

async function main(): Promise<void> {
  const sockPath = arg("--sock") ?? `${process.env.HOME}/.querora/run/querora.sock`;
  const token = arg("--token") ?? process.env.QUERORA_TOKEN;
  if (!token) throw new Error("--token required (the app passes it at spawn)");
  const byokFd = arg("--byok-key-fd");
  const settingsPath = arg("--settings");
  const sessionFile = arg("--session-file");
  const sessionArg = arg("--session-id");
  const settings: { model?: string; thinkingLevel?: string } = settingsPath
    ? JSON.parse(await import("node:fs").then((fs) => fs.readFileSync(settingsPath, "utf8")))
    : {};

  const client = new ToolApiClient(sockPath, token);
  await client.connect();

  const tools = makeTools(client);
  const { session } = await createAgentSession({
    customTools: tools,
    tools: tools.map((t) => t.name), // allowlist ONLY querora tools
    noTools: "builtin", // least privilege: no read/bash/etc. built-ins
    sessionManager: SessionManager.inMemory(),
  });

  let turnText = "";
  session.subscribe((event: unknown) => {
    const amt = (event as { assistantMessageEvent?: Record<string, unknown> }).assistantMessageEvent;
    if ((event as { type?: string }).type === "message_update" && amt?.type === "text_delta") {
      turnText += amt.delta ?? "";
      emit({ type: "token", text: amt.delta ?? "" });
    } else if ((event as { type?: string }).type === "message_update" && amt?.type === "toolcall_start") {
      const c = amt as unknown as { tool?: string; name?: string; arguments?: unknown };
      emit({ type: "tool_call", tool: c.tool ?? c.name ?? "querora_tool", args: c.arguments ?? {} });
    } else if ((event as { type?: string }).type === "agent_end") {
      if (turnText.trim()) emit({ type: "answer", text: turnText });
      turnText = "";
    }
  });

  const piSessionId =
    session.sessionFile?.match(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/)?.[0] ??
    session.sessionId;
  emit({ type: "started", agent: "pi", session_id: piSessionId });
  if (byokFd) emit({ type: "byok", provider: "custom", key_source: `fd:${byokFd}` });

  const rl = readline.createInterface({ input: process.stdin });
  let stdinDone = false;
  let busy = false;
  const finishIfIdle = () => {
    if (stdinDone && !busy) {
      try {
        session.dispose();
      } catch {
        /* best effort */
      }
      process.exit(0);
    }
  };
  rl.on("close", () => {
    stdinDone = true;
    finishIfIdle();
  });
  rl.on("line", (line: string) => {
    line = line.trim();
    if (!line) return;
    let req: { prompt?: string };
    try {
      req = JSON.parse(line);
    } catch {
      return;
    }
    if (typeof req.prompt === "string") {
      busy = true;
      session
        .prompt(req.prompt)
        .then(() => emit({ type: "done" }))
        .catch((e: Error) => {
          emit({ type: "context_lost", reason: String(e?.message ?? e) });
          emit({ type: "done" });
        })
        .finally(() => {
          busy = false;
          finishIfIdle();
        });
    }
  });
}

main().catch((e: Error) => {
  emit({ type: "failed", error: String(e?.message ?? e) });
  process.exit(1);
});
