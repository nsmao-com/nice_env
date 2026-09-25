"use client";

import * as React from "react";
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "./api";
import type { DownloadProgress } from "@nsb/schema";
import { normalizeError, type AppErrorShape } from "./backend";
import { toast } from "sonner";
import { useInstallTasks } from "./install-tasks";

/* 服务状态轮询：2s，不阻塞。
   initialDataUpdatedAt: 0 让 react-query 立刻发起首次请求——否则 initialData 的空数组
   会被当成「新鲜数据」，在 staleTime 内不请求，页面就先显示成空的。 */
export function useServices(intervalMs = 2000) {
  return useQuery({
    queryKey: ["services"],
    queryFn: api.listServiceStatus,
    refetchInterval: intervalMs,
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

export function useService(id: string | undefined, intervalMs = 2000) {
  return useQuery({
    queryKey: ["services"],
    queryFn: api.listServiceStatus,
    refetchInterval: intervalMs,
    select: (list) => list.find((s) => s.id === id),
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

export function usePackages() {
  return useQuery({
    queryKey: ["packages"],
    queryFn: api.listPackages,
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

/**
 * 远程版本目录：返回 id → 该包从上游枚举到的完整版本列表。
 * 后端带 6 小时缓存，这里 staleTime 设长一些避免重复请求；
 * 「刷新版本」用 refresh() 强制绕过缓存。
 */
export function useVersionCatalogs() {
  const qc = useQueryClient();
  const query = useQuery({
    queryKey: ["version-catalogs"],
    queryFn: () => api.versionCatalogs(false),
    staleTime: 5 * 60_000,
    initialData: [] as Awaited<ReturnType<typeof api.versionCatalogs>>,
    initialDataUpdatedAt: 0,
    retry: 0,
  });
  const byId = React.useMemo(() => {
    const m = new Map<string, (typeof query.data)[number]>();
    for (const c of query.data) m.set(c.id, c);
    return m;
  }, [query.data]);
  const refresh = React.useCallback(async () => {
    await qc.fetchQuery({
      queryKey: ["version-catalogs"],
      queryFn: () => api.versionCatalogs(true),
      staleTime: 0,
    });
  }, [qc]);
  return { ...query, byId, refresh };
}

export function useSites() {
  return useQuery({
    queryKey: ["sites"],
    queryFn: api.listSites,
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

/** 服务栈列表（用户自定义的一整套服务） */
export function useStacks() {
  return useQuery({
    queryKey: ["stacks"],
    queryFn: api.listStacks,
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

export function useSystemStats(intervalMs = 2000) {
  return useQuery({
    queryKey: ["system-stats"],
    queryFn: api.getSystemStats,
    refetchInterval: intervalMs,
  });
}

export function useCerts() {
  return useQuery({ queryKey: ["certs"], queryFn: api.listCerts, initialDataUpdatedAt: 0, initialData: [] });
}

export function useHosts() {
  return useQuery({ queryKey: ["hosts"], queryFn: api.readHosts, initialDataUpdatedAt: 0, initialData: [] });
}

/** 当前端口方案对应的端口表。站点 URL / 数据库连接串都必须用它，
 *  否则用户切到「标准档（80/3306）」后界面上的地址全是错的。
 *  用户在设置里逐个改过的端口（portOverrides）优先于档位默认值。 */
export function usePorts() {
  const { data: settings } = useSettings();
  const profile = settings?.portProfile === "safe" ? ("safe" as const) : ("standard" as const);
  const overrides = settings?.portOverrides;
  return React.useMemo(() => {
    // portOverrides 是每次 getSettings 新建的对象：先序列化成稳定 key 再参与 memo，
    // 否则每个渲染周期都会重算（并让下游 useMemo 全部失效）
    const merged = { ...expectedPorts(profile) };
    if (overrides) {
      for (const [k, v] of Object.entries(overrides)) {
        if (typeof v === "number" && k in merged) {
          (merged as Record<string, number>)[k] = v;
        }
      }
    }
    return merged;
  }, [profile, JSON.stringify(overrides ?? {})]);
}

/** 环境变量注入状态（PATH 托管目录 + 每个包可用的命令） */
export function usePathEnv() {
  return useQuery({
    queryKey: ["pathenv"],
    queryFn: api.pathenvStatus,
    staleTime: 5_000,
  });
}

export function useSettings() {
  return useQuery({
    queryKey: ["settings"],
    queryFn: api.getSettings,
    staleTime: 30_000,
  });
}

export function useDatabases() {
  return useQuery({ queryKey: ["databases"], queryFn: api.dbList, initialDataUpdatedAt: 0, initialData: [] });
}

export function useDbUsers() {
  return useQuery({ queryKey: ["db-users"], queryFn: api.dbUsers, initialDataUpdatedAt: 0, initialData: [] });
}

/* 日志 tail 轮询 */
export function useLogTail(id: string | null, intervalMs = 1500, lines = 500, enabled = true) {
  const [data, setData] = useState<string[]>([]);
  const [error, setError] = useState<AppErrorShape | null>(null);
  useEffect(() => {
    if (!id || !enabled) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      try {
        const got = await api.tailLogs(id, lines);
        if (alive) {
          setData(got.map((l) => l.line));
          setError(null);
        }
      } catch (e) {
        if (alive) setError(normalizeError(e));
      }
      if (alive) timer = setTimeout(tick, intervalMs);
    };
    tick();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [id, intervalMs, lines, enabled]);
  return { lines: data, error };
}

/* 下载进度事件（真实后端事件；浏览器下恒 null）。
   事件由 InstallTasksBridge 全局订阅一次写进 store，这里只读——
   以前每个调用方各自 listen，套件页每一行都订阅一份，一个进度事件就让整页重渲染。
   只关心某个套件时优先用 useInstallTasks + activeProgressFor 精确取值。 */
export function useDownloadProgress(): Record<string, DownloadProgress> {
  return useInstallTasks((s) => s.progress);
}

/* 统一错误 toast */
export function toastError(e: unknown, fallback = "操作失败") {
  const err = normalizeError(e);
  toast.error(err.message || fallback, {
    description: err.hint,
  });
}

/**
 * 端口冲突专用错误 toast：带上「结束占用进程并重试」的动作按钮。
 * 后端把冲突端口与占用 pid 一起塞进 AppError，这里直接拿来用。
 * 返回 true = 确实弹了端口冲突 toast（调用方可跳过普通提示）。
 */
export function toastPortConflict(
  e: unknown,
  opts: { onResolved?: () => void; retryLabel?: string } = {}
): boolean {
  const err = normalizeError(e) as AppErrorShape & { port?: number; pid?: number; holder?: string };
  if (err.code !== "PORT_IN_USE" && !err.port) return false;
  const port = err.port;
  toast.error(err.message || "端口被占用", {
    description: err.hint,
    duration: 12000,
    action:
      port != null
        ? {
            label: opts.retryLabel ?? "结束占用并重试",
            onClick: async () => {
              try {
                await api.closePort(port);
                toast.success(`端口 ${port} 已释放`);
                opts.onResolved?.();
              } catch (e2) {
                toastError(e2, "结束占用进程失败");
              }
            },
          }
        : undefined,
  });
  return true;
}

export function useInvalidate() {
  const qc = useQueryClient();
  return (...keys: string[]) =>
    keys.forEach((k) => qc.invalidateQueries({ queryKey: [k] }));
}

/* 端口方案 → 期望端口 */
export function expectedPorts(profile: "safe" | "standard") {
  return profile === "safe"
    ? { http: 8080, https: 8443, mysql: 23306, redis: 26379, apacheHttp: 8180, apacheHttps: 8444, postgres: 25432, mongodb: 28017 }
    : { http: 80, https: 443, mysql: 3306, redis: 6379, apacheHttp: 8080, apacheHttps: 8443, postgres: 5432, mongodb: 27017 };
}

/* 站点 URL 拼装 */
export function siteUrl(site: { domains: string[]; https: boolean }, httpPort: number, httpsPort: number) {
  const domain = site.domains[0] ?? "localhost";
  const standard = site.https ? httpsPort === 443 : httpPort === 80;
  const port = site.https ? httpsPort : httpPort;
  return `${site.https ? "https" : "http"}://${domain}${standard ? "" : `:${port}`}`;
}

/* 复制到剪贴板 */
export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast.success("已复制");
  } catch {
    toast.error("复制失败");
  }
}

/* 简易 useInterval */
export function useInterval(fn: () => void, ms: number | null) {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    if (ms === null) return;
    const id = setInterval(() => ref.current(), ms);
    return () => clearInterval(id);
  }, [ms]);
}
