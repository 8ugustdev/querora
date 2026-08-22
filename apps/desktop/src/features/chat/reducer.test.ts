/* SPDX-License-Identifier: Apache-2.0 */
import { describe, expect, it } from "vitest";
import { chatReducer, initialChatState, type ChatAction, type ChatMessage } from "./reducer";
import type { AgentEvent } from "../../lib/contracts";

/** FakeDriver event script: the canonical "revenue by month" turn. */
export function fakeDriverTurn(): AgentEvent[] {
  return [
    { type: "started", value: { agent: "claude", session_id: "s-1" } },
    { type: "token", value: { text: "Let me check the semantics." } },
    { type: "tool_call", value: { tool: "mcp__querora__search_semantics", args: { query: "revenue" } } },
    { type: "tool_result", value: { tool: "search_semantics", ok: true, summary: "3 hits" } },
    { type: "tool_call", value: { tool: "mcp__querora__execute_query", args: { ir: { source: "shop" } } } },
    { type: "tool_result", value: { tool: "execute_query", ok: true, summary: "4 rows" } },
    { type: "token", value: { text: " Revenue grew steadily." } },
    { type: "answer", value: { text: "Let me check the semantics. Revenue grew steadily." } },
    { type: "done" },
  ] as unknown as AgentEvent[];
}

function run(script: AgentEvent[]): ReturnType<typeof chatReducer> {
  let state = chatReducer(initialChatState, { type: "user_send", text: "monthly revenue?" });
  for (const ev of script) {
    state = chatReducer(state, { type: "agent_event", ev } as ChatAction);
  }
  return chatReducer(state, { type: "agent_end" });
}

describe("chat reducer (FakeDriver)", () => {
  it("streams tokens into the agent message", () => {
    const s = run(fakeDriverTurn());
    const agent = s.messages.at(-1)!;
    expect(agent.role).toBe("agent");
    expect(agent.streaming).toBe(false);
    expect(agent.text).toContain("Revenue grew steadily.");
  });

  it("records the tool timeline in order", () => {
    const s = run(fakeDriverTurn());
    const tl = s.messages.at(-1)!.toolTimeline;
    expect(tl.map((t: { kind: string }) => t.kind)).toEqual(["call", "result", "call", "result"]);
    expect(tl[1].tool).toBe("search_semantics");
    expect(tl[1].ok).toBe(true);
  });

  it("marks busy through the turn and clear at end", () => {
    let state = chatReducer(initialChatState, { type: "user_send", text: "q" });
    expect(state.busy).toBe(true);
    state = chatReducer(state, { type: "agent_end" });
    expect(state.busy).toBe(false);
  });

  it("captures result_id from execute_query tool results and attaches charts", () => {
    let state = chatReducer(initialChatState, { type: "user_send", text: "q" });
    const toolResult = {
      type: "tool_result",
      value: { tool: "execute_query", ok: true, summary: '...', result_id: "r-123" },
    } as unknown as AgentEvent;
    state = chatReducer(state, { type: "agent_event", ev: toolResult });
    const last = state.messages.at(-1) as ChatMessage;
    expect(last.pendingResultIds).toEqual(["r-123"]);
    const fakeResult = {
      result_id: "r-123",
      columns: ["m", "v"],
      column_types: ["string", "number"],
      rows: [],
      sql: "SELECT 1",
      params: [],
      semantic_version: "v",
      stats: { row_count: 0, duration_ms: 1, row_cap: 10, timeout_secs: 5 },
    };
    state = chatReducer(state, { type: "attach_result", result: fakeResult as never, spec: undefined });
    expect((state.messages.at(-1) as ChatMessage).charts).toHaveLength(1);
  });

  it("surfaces context_lost as a banner flag", () => {
    let state = chatReducer(initialChatState, { type: "user_send", text: "q" });
    state = chatReducer(state, {
      type: "agent_event",
      ev: { type: "context_lost", value: { reason: "agent upgraded" } } as unknown as AgentEvent,
    });
    expect(state.messages.at(-1)!.contextLost).toContain("agent upgraded");
  });

  it("garbage events never throw", () => {
    let state = chatReducer(initialChatState, { type: "user_send", text: "q" });
    for (const ev of [{ type: "???" }, { type: "token", value: {} }, null, 42]) {
      expect(() =>
        chatReducer(state, { type: "agent_event", ev: ev as unknown as AgentEvent }),
      ).not.toThrow();
    }
  });
});
