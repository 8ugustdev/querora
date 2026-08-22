/* SPDX-License-Identifier: Apache-2.0 */
import type { AgentEvent, QueryResult, VisualizationSpec } from "../../lib/contracts";

/** One message in the chat view. */
export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  text: string;
  toolTimeline: Array<{ kind: string; tool: string; ok?: boolean; summary?: string }>;
  contextLost?: string;
  streaming: boolean;
  /** Charts rendered under this message (filled when queries completed). */
  charts: Array<{ result: QueryResult; spec?: VisualizationSpec }>;
  /** result_ids seen in tool results this turn (fetched on completion). */
  pendingResultIds?: string[];
}

/** A tool result carrying a result_id the trust panel can expand. */
let nextId = 0;
const uid = () => `m${++nextId}`;

/** Reducer state for one chat session view. */
export interface ChatState {
  messages: ChatMessage[];
  busy: boolean;
}

export type ChatAction =
  | { type: "hydrate"; messages: ChatMessage[] }
  | { type: "attach_result"; result: QueryResult; spec?: VisualizationSpec }
  | { type: "user_send"; text: string }
  | { type: "agent_start" }
  | { type: "agent_event"; ev: AgentEvent }
  | { type: "agent_end" };

export const initialChatState: ChatState = { messages: [], busy: false };

/**
 * Single source of truth: normalized AgentEvent → UI state. Pure; unit
 * tested without Tauri (the FakeDriver tests drive this reducer).
 */
export function chatReducer(state: ChatState, action: ChatAction): ChatState {
  switch (action.type) {
    case "hydrate":
      return { messages: action.messages, busy: false };
    case "user_send":
      return {
        ...state,
        busy: true,
        messages: [
          ...state.messages,
          { id: uid(), role: "user", text: action.text, toolTimeline: [], streaming: false, charts: [] },
          { id: uid(), role: "agent", text: "", toolTimeline: [], streaming: true, charts: [], pendingResultIds: [] },
        ],
      };
    case "agent_start":
      return state;
    case "attach_result": {
      const messages = [...state.messages];
      const last = messages[messages.length - 1];
      if (!last || last.role !== "agent") return state;
      messages[messages.length - 1] = {
        ...last,
        charts: [...last.charts, { result: action.result, spec: action.spec }],
      };
      return { ...state, messages };
    }
    case "agent_end":
      return {
        ...state,
        busy: false,
        messages: state.messages.map((m) => (m.streaming ? { ...m, streaming: false } : m)),
      };
    case "agent_event": {
      const ev = action.ev;
      if (!ev || typeof ev.type !== "string") return state;
      const messages = [...state.messages];
      const last = messages[messages.length - 1];
      if (!last || last.role !== "agent") return state;
      const next: ChatMessage = { ...last };
      switch (ev.type) {
        case "token":
          next.text += String(ev.value?.text ?? "");
          break;
        case "tool_call":
          next.toolTimeline = [...next.toolTimeline, { kind: "call", tool: ev.value.tool }];
          break;
        case "tool_result":
          next.toolTimeline = [...next.toolTimeline, { kind: "result", tool: ev.value.tool, ok: ev.value.ok, summary: ev.value.summary }];
          if (ev.value.result_id) next.pendingResultIds = [...(next.pendingResultIds ?? []), ev.value.result_id];
          break;
        case "answer":
          next.text = String(ev.value?.text ?? "");
          break;
        case "context_lost":
          next.contextLost = String(ev.value?.reason ?? "context lost");
          break;
        case "failed":
          next.text = next.text || `⚠ ${String(ev.value?.error ?? "agent failed")}`;
          break;
        case "done":
          next.streaming = false;
          break;
        default:
          return state;
      }
      messages[messages.length - 1] = next;
      return { ...state, messages };
    }
    default:
      return state;
  }
}
