"use client";

import { create } from "zustand";
import { toast } from "sonner";
import type { DownloadProgress } from "@nsb/schema";
import * as api from "./api";
import { normalizeError } from "./backend";
import type { TKey } from "./i18n";
import { useUI } from "./store";

/* ============================================================
   套件安装任务：与弹窗 / 页面解耦的全局后台任务。
   - 弹窗只是任务的一个「视图」，随时可关，关了安装照常跑完；
   - 同一版本重复发起只会复用正在进行的任务，不会并发装两次；
   - 下载进度事件全局只订阅一次（见 providers 里的 InstallTasksBridge），
     各处按 taskId 精确取值，进度刷新只重渲染相关的那一行。
   ============================================================ */

export type InstallStatus = "running" | "done" | "error";

export interface InstallTask {
  /** `${id}@${version}`（与后端下载进度的 taskId 一致）；不指定版本时就是 id，由后端取最新版 */
  key: string;
  id: string;
  version?: string;
  displayName: string;
  status: InstallStatus;
  error?: string;
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
  cancel: (key: string) => Promise<void>;
}

/** 进行中任务的 Promise（去重用，不进 state，避免无意义的重渲染） */
const inflight = new Map<string, Promise<boolean>>();
/** 用户主动取消的任务：失败回调里据此区分「取消」与「出错」 */
const cancelled = new Set<string>();

/** store 外取文案（toast 可能在弹窗/页面卸载后才触发，拿不到 useT） */
const t = (key: TKey) => useUI.getState().t(key);

export const useInstallTasks = create<InstallTasksState>()((set) => ({
  tasks: {},
  progress: {},

  setProgress: (p) => set((s) => ({ progress: { ...s.progress, [p.taskId]: p } })),

  start: (target, opts) => {
    const key = target.version ? `${target.id}@${target.version}` : target.id;
    const running = inflight.get(key);
    if (running) return running;

    cancelled.delete(key);
    set((s) => {
      // 清掉上一次（失败/已完成）残留的进度，免得重装时一上来就显示「已完成」
      const progress = { ...s.progress };
      delete progress[key];
      return {
        progress,
        tasks: {
          ...s.tasks,
          [key]: { key, id: target.id, version: target.version, displayName: target.displayName, status: "running" },
        },
      };
    });

    const label = target.version ? `${target.displayName} ${target.version}` : target.displayName;
    const promise = api
      .installPackage(key)
      .then(
        () => {
          set((s) => ({ tasks: { ...s.tasks, [key]: { ...s.tasks[key], status: "done", error: undefined } } }));
          if (!opts?.quiet) toast.success(`${label} ${t("packages.installed")}`);
          return true;
        },
        (e: unknown) => {
          if (cancelled.has(key)) {
            // 取消不是失败：直接移除任务，界面回到未安装状态
            set((s) => {
              const tasks = { ...s.tasks };
              delete tasks[key];
              return { tasks };
            });
            return false;
          }
          const err = normalizeError(e);
          const message = err.message ? `${err.message}${err.hint ? ` — ${err.hint}` : ""}` : String(e);
          set((s) => ({ tasks: { ...s.tasks, [key]: { ...s.tasks[key], status: "error", error: message } } }));
          toast.error(`${label} ${t("install.failed")}`, { description: message });
          return false;
        }
      )
      .finally(() => {
        inflight.delete(key);
        cancelled.delete(key);
      });
    inflight.set(key, promise);
    return promise;
  },

  cancel: async (key) => {
    // 非本 store 发起的下载（如新手引导）也照样取消，只是没有任务状态要收尾
    if (inflight.has(key)) cancelled.add(key);
    try {
      await api.cancelDownload(key);
      toast.info(t("install.cancelled"));
    } catch (e) {
      cancelled.delete(key);
      toast.error(normalizeError(e).message || t("install.failed"));
    }
  },
}));

/** 某个套件（任一版本）正在下载中的进度；用于列表行内进度条 */
export function activeProgressFor(progress: Record<string, DownloadProgress>, pkgId: string) {
  const prefix = `${pkgId}@`;
  for (const p of Object.values(progress)) {
    if (p.taskId.startsWith(prefix) && p.received < p.total) return p;
  }
  return undefined;
}
