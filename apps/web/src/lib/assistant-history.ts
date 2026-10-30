import type { AssistantMessage } from "./api";

export interface AssistantEntry { id: number; kind: "user" | "assistant" | "tool"; text: string; detail?: string; failed?: boolean }
export interface AssistantSession {
  id: string;
  title: string;
  updatedAt: number;
  entries: AssistantEntry[];
  history: AssistantMessage[];
  draft: string;
}
const key = "niceenv-assistant-sessions-v1";
export const MAX_SESSIONS = 20;
const MAX_BYTES = 3 * 1024 * 1024;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);

export function newAssistantSession(title: string): AssistantSession {
  return { id: crypto.randomUUID(), title, updatedAt: Date.now(), entries: [], history: [], draft: "" };
}

/** Interrupted tool calls become cancelled results, never executable persisted approvals. */
export function restoreAssistantHistory(input: unknown): AssistantMessage[] {
  if (!Array.isArray(input)) return [];
  const output: AssistantMessage[] = [];
  let pending = new Set<string>();
  const cancel = () => {
    for (const id of pending) output.push({ role: "tool", tool_call_id: id, content: '{"cancelled":true,"message":"Previous session was interrupted. Check current state before proposing any action again."}' });
    pending = new Set();
  };
  for (const item of input.slice(0, 96)) {
    if (!object(item) || !["user", "assistant", "tool"].includes(String(item.role))
      || !(typeof item.content === "string" || item.content === null)
      || (typeof item.content === "string" && item.content.length > 64 * 1024)) continue;
    if (item.role === "tool") {
      if (typeof item.tool_call_id === "string" && pending.delete(item.tool_call_id)) {
        output.push({ role: "tool", tool_call_id: item.tool_call_id, content: item.content });
      }
      continue;
    }
    cancel();
    const message: AssistantMessage = { role: item.role as "user" | "assistant", content: item.content };
    if (item.role === "assistant" && Array.isArray(item.tool_calls)) {
      const calls = item.tool_calls.slice(0, 8).flatMap((call) => {
        if (!object(call) || call.type !== "function" || typeof call.id !== "string" || !call.id || call.id.length > 200
          || pending.has(call.id) || !object(call.function) || typeof call.function.name !== "string"
          || call.function.name.length > 64 || typeof call.function.arguments !== "string" || call.function.arguments.length > 8192) return [];
        pending.add(call.id);
        return [{ id: call.id, type: "function" as const, function: { name: call.function.name, arguments: call.function.arguments } }];
      });
      if (calls.length) message.tool_calls = calls;
    }
    output.push(message);
  }
  cancel();
  return output;
}

export function readAssistantSessions(): { sessions: AssistantSession[]; activeId: string; warning: boolean } {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return { sessions: [], activeId: "", warning: false };
    if (new TextEncoder().encode(raw).length > MAX_BYTES) throw new Error("History exceeds limit");
    const data: unknown = JSON.parse(raw);
    if (!object(data) || !Array.isArray(data.sessions)) throw new Error("Invalid history");
    const ids = new Set<string>();
    const sessions = data.sessions.slice(0, MAX_SESSIONS).flatMap((row): AssistantSession[] => {
      if (!object(row) || typeof row.id !== "string" || !row.id || row.id.length > 100 || ids.has(row.id)
        || typeof row.title !== "string" || !Array.isArray(row.entries)) return [];
      ids.add(row.id);
      const entries = row.entries.slice(0, 256).flatMap((item, index): AssistantEntry[] => {
        if (!object(item) || !["user", "assistant", "tool"].includes(String(item.kind)) || typeof item.text !== "string") return [];
        return [{ id: index + 1, kind: item.kind as AssistantEntry["kind"], text: item.text.slice(0, 64 * 1024),
          ...(typeof item.detail === "string" ? { detail: item.detail.slice(0, 64 * 1024) } : {}), failed: item.failed === true }];
      });
      return [{ id: row.id, title: row.title.slice(0, 80), updatedAt: typeof row.updatedAt === "number" ? row.updatedAt : 0,
        entries, history: restoreAssistantHistory(row.history), draft: typeof row.draft === "string" ? row.draft.slice(0, 8000) : "" }];
    });
    return { sessions, activeId: typeof data.activeId === "string" ? data.activeId : "", warning: false };
  } catch { return { sessions: [], activeId: "", warning: true }; }
}

export function writeAssistantSessions(sessions: AssistantSession[], activeId: string): void {
  // Serialize only conversation fields. No connection settings, secrets or approval tokens.
  const raw = JSON.stringify({ activeId, sessions: sessions.map(({ id, title, updatedAt, entries, history, draft }) =>
    ({ id, title, updatedAt, entries, history, draft })) });
  if (sessions.length > MAX_SESSIONS || new TextEncoder().encode(raw).length > MAX_BYTES) {
    throw new Error("History storage limit reached");
  }
  localStorage.setItem(key, raw);
}
