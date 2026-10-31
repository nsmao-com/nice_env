/**
 * 后端适配层：桌面端只使用 Wails；浏览器预览使用 mock。
 * 前端禁止直接操作文件系统，一切经此层。
 */

export const desktopRuntime = typeof window === "undefined" ? "browser"
  : "__NICEENV_WAILS__" in window ? "wails" : "browser";
export const isDesktop = desktopRuntime !== "browser";

let wailsCommands: Promise<Set<string>> | undefined;
const bridgeMethod = "niceenv/internal/desktop.Bridge.Invoke";

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (desktopRuntime === "wails") {
    const { Call } = await import("@wailsio/runtime");
    try {
      if (cmd !== "backend_capabilities") {
        wailsCommands ??= Promise.resolve(Call.ByName(bridgeMethod, "backend_capabilities", {}))
          .then((value: { commands: string[] }) => new Set(value.commands))
          .catch((error: unknown) => { wailsCommands = undefined; throw error; });
        if (!(await wailsCommands).has(cmd)) throw {
          code: "COMMAND_NOT_PORTED", message: "此功能尚未迁移到 Go 后端", detail: cmd,
          hint: "当前是独立开发版本，不可替代现用正式版。",
        } satisfies AppErrorShape;
      }
      return await Call.ByName(bridgeMethod, cmd, args ?? {}) as T;
    }
    catch (error) { throw normalizeError(error); }
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

/** 网页导航与桌面端同一约束；保留查询参数，拒绝可执行协议和嵌入凭据。 */
export function browserUrl(input: string): string {
  const invalid = (): AppErrorShape => ({
    code: "BROWSER_URL_INVALID",
    message: "无法打开网页：请使用完整的 HTTP 或 HTTPS 地址",
    hint: "地址不能包含登录凭据、反斜杠或控制字符；本地文件请使用“打开所在文件夹”。",
  });
  if (/[\u0000-\u001f\u007f-\u009f\\]/.test(input) || !/^https?:\/\/[^/?#]/i.test(input.trim())) throw invalid();
  let url: URL;
  try { url = new URL(input.trim()); } catch { throw invalid(); }
  if (!url.hostname || url.username || url.password || url.port === "0") throw invalid();
  return url.href;
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
    if (anyE.cause && typeof anyE.cause === "object") {
      const cause = anyE.cause as Record<string, unknown>;
      if (typeof cause.code === "string" && typeof cause.message === "string") return cause as unknown as AppErrorShape;
    }
    if (typeof anyE.message === "string") return { code: "UNKNOWN", message: anyE.message };
  }
  return { code: "UNKNOWN", message: String(e) };
}

/**
 * 浏览器本地事件总线：桌面端事件由原生后端发出，浏览器（mock）没有 IPC，
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

/** 订阅原生事件（下载进度等）；浏览器下订阅本地事件总线 */
export async function listen<T>(
  event: string,
  handler: (payload: T) => void
): Promise<() => void> {
  if (desktopRuntime === "wails") {
    const { Events } = await import("@wailsio/runtime");
    return Events.On(event, (ev) => handler(ev.data as T));
  }
  const set = localBus.get(event) ?? new Set<LocalHandler>();
  const h: LocalHandler = (p) => handler(p as T);
  set.add(h);
  localBus.set(event, set);
  return () => set.delete(h);
}
