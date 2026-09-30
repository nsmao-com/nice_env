"use client";

import { create } from "zustand";
import { toast } from "sonner";
import { PackageInstallResult, type DownloadProgress } from "@nsb/schema";
import * as api from "./api";
import { normalizeError, type AppErrorShape } from "./backend";
import type { TKey } from "./i18n";
import { useUI } from "./store";
import { sameVersion } from "./utils";

/* ============================================================
   套件安装任务：与弹窗 / 页面解耦的全局后台任务。
   - 弹窗只是任务的一个「视图」，随时可关，关了安装照常跑完；
   - 同一版本重复发起只会复用正在进行的任务，不会并发装两次；
   - 下载进度事件全局只订阅一次（见 providers 里的 InstallTasksBridge），
     各处按 taskId 精确取值，进度刷新只重渲染相关的那一行。
   ============================================================ */

export type InstallStatus = "running" | "done" | "error" | "cancelled";

export interface InstallTask {
  /** `${id}@${version}`（与后端下载进度的 taskId 一致）；不指定版本时就是 id，由后端取最新版 */
  key: string;
  /** 每次安装独立标识，取消和进度只能作用于这一轮请求。 */
  requestId: string;
  id: string;
  version?: string;
  displayName: string;
  status: InstallStatus;
  /** 进行中由进度提示，完成后始终使用安装命令返回的实际版本。 */
  resolvedVersion?: string;
  progressId?: string;
  progressKeys?: string[];
  startedAt: number;
  error?: string;
  /** 安装成功后的待处理项，不应被当成安装失败而重复下载。 */
  pathSyncError?: AppErrorShape;
  cancelRequested?: boolean;
}

interface InstallTasksState {
  tasks: Record<string, InstallTask>;
  /** 下载 / 安装进度（按 taskId） */
  progress: Record<string, DownloadProgress>;
  setProgress: (p: DownloadProgress) => void;
  /** 发起后台安装；已在进行中的同一版本直接复用。成功 resolve true，失败/取消 resolve false。
   *  quiet：不弹「安装完成」（调用方自己汇总提示）；失败提示始终会弹，免得转入后台后无人知晓 */
  start: (
    target: { id: string; version?: string; displayName: string },
    opts?: { quiet?: boolean }
  ) => Promise<boolean>;
  cancel: (key: string) => Promise<boolean>;
  dismiss: (key: string) => void;
  updatePathSyncError: (task: InstallTask, error?: AppErrorShape) => void;
}

/** 进行中任务的 Promise（去重用，不进 state，避免无意义的重渲染） */
const inflight = new Map<string, Promise<boolean>>();

/** store 外取文案（toast 可能在弹窗/页面卸载后才触发，拿不到 useT） */
const t = (key: TKey) => useUI.getState().t(key);

/** 自动选版与精确版本可能先后共用进度键；不得清理其他仍在运行任务的进度。 */
function clearTaskProgress(state: InstallTasksState, key: string) {
  const requestId = state.tasks[key]?.requestId;
  const owned = new Set([key, ...(state.tasks[key]?.progressKeys ?? [])]);
  for (const task of Object.values(state.tasks)) {
    if (task.key !== key && task.status === "running") {
      owned.delete(task.key);
      for (const progressKey of task.progressKeys ?? []) owned.delete(progressKey);
    }
  }
  return Object.fromEntries(Object.entries(state.progress).filter(([id, progress]) =>
    (!requestId || progress.requestId !== requestId) && !owned.has(id)));
}

export const useInstallTasks = create<InstallTasksState>()((set, get) => ({
  tasks: {},
  progress: {},

  setProgress: (p) => set((s) => {
    const task = p.requestId ? Object.values(s.tasks).find((task) => task.status === "running"
      && task.requestId === p.requestId && (p.taskId === task.id || p.taskId.startsWith(`${task.id}@`))) : undefined;
    if (p.requestId && !task) return s; // 旧请求的延迟事件不能覆盖重装任务。
    if (!task && Object.values(s.tasks).some((task) => task.status === "running"
      && task.requestId === s.progress[p.taskId]?.requestId)) return s;
    const resolvedVersion = task && p.taskId.startsWith(`${task.id}@`) ? p.taskId.slice(task.id.length + 1) : task?.resolvedVersion;
    const changed = task && (task.progressId !== p.taskId || task.resolvedVersion !== resolvedVersion);
    return {
      progress: { ...s.progress, [p.taskId]: p },
      ...(changed ? { tasks: { ...s.tasks, [task.key]: { ...task, resolvedVersion, progressId: p.taskId,
        progressKeys: [...new Set([...(task.progressKeys ?? []), p.taskId])] } } } : {}),
    };
  }),

  start: (target, opts) => {
    const key = target.version ? `${target.id}@${target.version}` : target.id;
    const running = inflight.get(key);
    if (running) return running;
    const requestId = crypto.randomUUID();

    set((s) => {
      // 清掉上一次（失败/已完成）残留的进度，免得重装时一上来就显示「已完成」
      const progress = clearTaskProgress(s, key);
      return {
        progress,
        tasks: {
          ...s.tasks,
          [key]: { key, requestId, id: target.id, version: target.version, displayName: target.displayName, status: "running", startedAt: Date.now() },
        },
      };
    });

    const label = target.version ? `${target.displayName} ${target.version}` : target.displayName;
    const promise = api
      .installPackage(key, requestId)
      .then((installed) => {
        const result = PackageInstallResult.safeParse(installed);
        if (!result.success || result.data.id !== target.id || (target.version
          && !sameVersion(result.data.version, target.version))) {
          throw { code: "INSTALL_RESULT_UNCONFIRMED", message: t("install.resultUnconfirmed") };
        }
        const resolvedVersion = result.data.version;
        const progressId = `${result.data.id}@${resolvedVersion}`;
        set((s) => ({ tasks: { ...s.tasks, [key]: { ...s.tasks[key], status: "done", resolvedVersion, progressId,
          progressKeys: [...new Set([...(s.tasks[key]?.progressKeys ?? []), progressId])], cancelRequested: false, error: undefined,
          pathSyncError: result.data.pathSyncError } } }));
        if (result.data.pathSyncError) {
          toast.warning(`${target.displayName} ${resolvedVersion} · ${t("install.pathSyncPending")}`, {
            description: t("install.pathSyncHint"),
          });
        } else if (!opts?.quiet) toast.success(`${target.displayName} ${resolvedVersion} ${t("packages.installed")}`);
        return true;
      })
      .catch((e: unknown) => {
        const err = normalizeError(e);
        if (err.code === "CANCELLED") {
          // 以后端确认结果为准；点击取消本身不能冒充任务已停止。
          set((s) => ({ tasks: { ...s.tasks, [key]: { ...s.tasks[key], status: "cancelled", cancelRequested: false } } }));
          toast.info(`${label} ${t("install.cancelled")}`);
          return false;
        }
        const message = err.message ? `${err.message}${err.hint ? ` — ${err.hint}` : ""}` : String(e);
        set((s) => ({ tasks: { ...s.tasks, [key]: { ...s.tasks[key], status: "error", cancelRequested: false, error: message } } }));
        toast.error(`${label} ${t("install.failed")}`, {
          description: message,
          classNames: { description: "line-clamp-2 [overflow-wrap:anywhere]" },
        });
        return false;
      })
      .finally(() => {
        inflight.delete(key);
        set((s) => ({ progress: clearTaskProgress(s, key) }));
      });
    inflight.set(key, promise);
    return promise;
  },

  cancel: async (key) => {
    const task = get().tasks[key];
    if (!task || task.status !== "running" || task.cancelRequested) return false;
    const current = () => get().tasks[key]?.requestId === task.requestId && get().tasks[key]?.status === "running";
    const markRequested = (requested: boolean) => set((s) => s.tasks[key]?.requestId === task.requestId && s.tasks[key]?.status === "running"
      ? { tasks: { ...s.tasks, [key]: { ...s.tasks[key], cancelRequested: requested } } } : s);
    markRequested(true);
    try {
      const accepted = await api.cancelDownload(task.progressId ?? key, task.requestId);
      if (!current()) return accepted;
      if (!accepted) {
        markRequested(false);
        toast.info(t("install.cannotCancel"));
      }
      return accepted;
    } catch (e) {
      if (current()) {
        markRequested(false);
        toast.error(normalizeError(e).message || t("install.failed"));
      }
      return false;
    }
  },

  dismiss: (key) => set((s) => {
    if (!s.tasks[key] || s.tasks[key].status === "running" || inflight.has(key)) return s;
    const tasks = { ...s.tasks };
    delete tasks[key];
    return { tasks };
  }),

  updatePathSyncError: (task, error) => set((s) => {
    // 旧重试结果不能清理重装后的新任务，也不能重新创建已移除的记录。
    if (s.tasks[task.key] !== task || task.status !== "done") return s;
    return { tasks: { ...s.tasks, [task.key]: { ...task, pathSyncError: error } } };
  }),
}));

/** 请求键和后端进度键可能不同（自动选版）；只读取已确认属于该任务的事件。 */
export function progressForTask(progress: Record<string, DownloadProgress>, task: InstallTask | undefined) {
  const item = task?.status === "running" ? progress[task.progressId ?? task.key] : undefined;
  return item?.requestId === task?.requestId ? item : undefined;
}

/** 某个套件（任一版本）正在下载中的进度；用于列表行内进度条 */
export function activeProgressFor(progress: Record<string, DownloadProgress>, pkgId: string) {
  const prefix = `${pkgId}@`;
  for (const p of Object.values(progress)) {
    if (p.taskId.startsWith(prefix) && ["downloading", "downloaded", "verifying", "extracting", "configuring"].includes(p.state)) return p;
  }
  return undefined;
}
