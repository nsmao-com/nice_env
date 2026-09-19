/**
 * 后端适配层：桌面端走 Tauri invoke；浏览器开发走 mock（数据结构即最终 schema）。
 * 前端禁止直接操作文件系统，一切经此层。
 */

export const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  }
  const { mockInvoke } = await import("./mock");
  return mockInvoke<T>(cmd, args);
}

export interface AppErrorShape {
  code: string;
  message: string;
  hint?: string;
  detail?: string;
}

/** 把 invoke 抛出的任意错误规整为 AppErrorShape（人话 + 建议） */
export async function safe<T>(p: Promise<T>): Promise<T> {
  try {
    return await p;
  } catch (e) {
    throw normalizeError(e);
  }
}

export function normalizeError(e: unknown): AppErrorShape {
  if (typeof e === "string") {
    try {
      const parsed = JSON.parse(e) as AppErrorShape;
      if (parsed && parsed.code && parsed.message) return parsed;
    } catch {
      /* 非结构化错误 */
    }
    return { code: "UNKNOWN", message: e };
  }
  if (e && typeof e === "object") {
    const anyE = e as Record<string, unknown>;
    if (anyE.code && anyE.message) return e as AppErrorShape;
  }
  return { code: "UNKNOWN", message: String(e) };
}

/**
 * 浏览器本地事件总线：桌面端事件由 Rust emit，浏览器（mock）没有 IPC，
 * 就在这里用同一个名字转发，让「下载进度」这类 UI 在 dev 下也能看到效果。
 */
type LocalHandler = (payload: unknown) => void;
const localBus = new Map<string, Set<LocalHandler>>();

export function emitLocal(event: string, payload: unknown) {
  localBus.get(event)?.forEach((h) => {
    try {
      h(payload);
    } catch {
      /* 单个订阅者出错不影响其它订阅者 */
    }
  });
}

/** 订阅 Rust 侧事件（下载进度等）；浏览器下订阅本地事件总线 */
export async function listen<T>(
  event: string,
  handler: (payload: T) => void
): Promise<() => void> {
  if (isTauri) {
    const { listen } = await import("@tauri-apps/api/event");
    const un = await listen<T>(event, (ev) => handler(ev.payload));
    return un;
  }
  const set = localBus.get(event) ?? new Set<LocalHandler>();
  const h: LocalHandler = (p) => handler(p as T);
  set.add(h);
  localBus.set(event, set);
  return () => set.delete(h);
}
