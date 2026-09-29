"use client";

import type { DatabaseEngine } from "@nsb/schema";

import * as React from "react";
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import * as api from "./api";
import type { DownloadProgress, VersionCatalog, Site, ServiceStatus, Stack, StackStartReport, BulkReport } from "@nsb/schema";
import { normalizeError, type AppErrorShape } from "./backend";
import { toast } from "sonner";
import { useInstallTasks } from "./install-tasks";
import { useT } from "./store";

/* 服务状态轮询：2s，不阻塞。本地 IPC 查询在断网时仍应执行。
   initialDataUpdatedAt: 0 让 react-query 立刻发起首次请求——否则 initialData 的空数组
   会被当成「新鲜数据」，在 staleTime 内不请求，页面就先显示成空的。 */
export function useServices(intervalMs = 2000) {
  return useQuery({
    queryKey: ["services"],
    queryFn: api.listServiceStatus,
    networkMode: "always",
    refetchInterval: intervalMs,
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

export function useService(id: string | undefined, intervalMs = 2000) {
  return useQuery({
    queryKey: ["services"],
    queryFn: api.listServiceStatus,
    networkMode: "always",
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

/** 同步所有套件列表缓存；较早返回的批量目录不能覆盖已刷新的单项。 */
async function publishVersionCatalogs(qc: QueryClient, incoming: VersionCatalog[], replace: boolean) {
  const filters = { queryKey: ["version-catalogs"], predicate: (query: { queryKey: readonly unknown[] }) => Array.isArray(query.queryKey[1]) };
  await qc.cancelQueries(filters, { silent: true });
  qc.setQueriesData<VersionCatalog[]>(filters, (previous) => {
    // 单项结果不能冒充完整的首次加载；下方重新读取仍为空的活动列表。
    if (!previous && !replace) return undefined;
    const old = new Map((previous ?? []).map((catalog) => [catalog.id, catalog]));
    const next = replace ? new Map<string, VersionCatalog>() : new Map(old);
    for (const catalog of incoming) {
      const current = old.get(catalog.id);
      next.set(catalog.id, current?.cachedAt != null && catalog.cachedAt != null && current.cachedAt > catalog.cachedAt ? current : catalog);
    }
    return [...next.values()].sort((a, b) => a.id.localeCompare(b.id));
  });
  await qc.refetchQueries({ ...filters, type: "active", predicate: (query) => filters.predicate(query) && query.state.data === undefined });
}

/** 项目 LTS 刷新与套件菜单使用同一入口，不创建旧的单项缓存键。 */
export async function refreshVersionCatalog(qc: QueryClient, id: string) {
  const catalog = await api.versionCatalog(id, true);
  await publishVersionCatalogs(qc, [catalog], false);
  return catalog;
}

/**
 * 远程版本目录：返回 id → 该包从上游枚举到的完整版本列表。
 * 后端带 6 小时缓存，这里 staleTime 设长一些避免重复请求；
 * 「刷新版本」用 refresh() 强制绕过缓存。
 */
export function useVersionCatalogs(packageIds: string[]) {
  const qc = useQueryClient();
  const ids = [...new Set(packageIds)].sort();
  // 后端已有批量目录命令；首屏只走一次 IPC，避免套件数量增加后产生
  // N 个独立请求、重复读取缓存以及 GitHub/上游匿名限流。
  // 把当前套件 ID 放进 key，远端清单新增套件时会自动重新拉取目录。
  const queryKey = ["version-catalogs", ids] as const;
  const query = useQuery({
    queryKey,
    queryFn: () => api.versionCatalogs(false),
    staleTime: 5 * 60_000,
    retry: false,
    enabled: ids.length > 0,
  });
  const catalogs = new Map((query.data ?? []).map((catalog) => [catalog.id, catalog]));
  const byId = new Map<string, VersionCatalog & { loading: boolean }>();
  ids.forEach((id) => {
    const catalog = catalogs.get(id);
    byId.set(id, {
      id, remote: [], online: false,
      ...catalog,
      ...(query.error ? { online: false, error: normalizeError(query.error).message } : {}),
      loading: query.isFetching,
    });
  });
  const refresh = React.useCallback(async (id: string) => {
    try {
      await refreshVersionCatalog(qc, id);
    } catch (error) {
      toastError(error);
    }
  }, [qc]);
  const refreshAll = React.useCallback(async () => {
    const catalogs = await api.versionCatalogs(true);
    await publishVersionCatalogs(qc, catalogs, true);
    return catalogs;
  }, [qc]);
  return { byId, refresh, refreshAll };
}

export function useSites() {
  return useQuery({
    queryKey: ["sites"],
    queryFn: api.listSites,
    networkMode: "always",
    refetchInterval: 4000,
    initialDataUpdatedAt: 0,
    initialData: [],
  });
}

/** 服务栈列表（用户自定义的一整套服务） */
export function useStacks() {
  return useQuery({
    queryKey: ["stacks"],
    queryFn: api.listStacks,
    networkMode: "always",
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

export function useDatabases(version?: string, enabled = true, engine: DatabaseEngine = "mysql") {
  return useQuery({ queryKey: ["databases", engine, version], queryFn: () => api.dbList(version, engine), enabled, retry: false });
}

export function useDbUsers(version?: string, enabled = true, engine: DatabaseEngine = "mysql") {
  return useQuery({ queryKey: ["db-users", engine, version], queryFn: () => api.dbUsers(version, engine), enabled, retry: false });
}

/* 日志按来源和行数隔离；暂停仅停止轮询，首次/切换/手动刷新仍可读取。 */
export function useLogTail(id: string | null, intervalMs = 1500, lines = 500, enabled = true) {
  const query = useQuery({
    queryKey: ["log-tail", id, lines],
    queryFn: async () => (await api.tailLogs(id!, lines)).map((line) => line.line),
    enabled: !!id,
    refetchInterval: enabled ? intervalMs : false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    retry: false,
    gcTime: 0,
  });
  return {
    lines: query.data ?? [],
    error: query.error ? normalizeError(query.error) : null,
    loading: !!id && query.isPending,
    refreshing: query.isFetching,
    refresh: query.refetch,
  };
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
  opts: { onResolved?: () => void | Promise<void>; retryLabel?: string; resolve?: () => Promise<void> } = {}
): boolean {
  const err = normalizeError(e) as AppErrorShape & { port?: number; pid?: number; holder?: string };
  if (err.code !== "PORT_IN_USE") return false;
  const port = err.port;
  let resolving = false;
  toast.error(err.message || "端口被占用", {
    description: err.hint,
    duration: 12000,
    action:
      port != null
        ? {
            label: opts.retryLabel ?? (opts.onResolved || opts.resolve ? "结束占用并重试" : "结束占用进程"),
            onClick: async () => {
              if (resolving) return;
              resolving = true;
              // 服务入口自行持有操作锁并复查目标，再处理端口及原动作。
              if (opts.resolve) {
                try { await opts.resolve(); } catch (error) { toastError(error); }
                finally { resolving = false; }
                return;
              }
              const pending = toast.loading(`正在释放端口 ${port}…`);
              try {
                await api.resolvePortConflict(port, err.pid);
                toast.success(`端口 ${port} 已释放`, { id: pending });
                await opts.onResolved?.();
              } catch (e2) {
                toast.dismiss(pending);
                toastError(e2, "端口处理或重试失败");
              } finally {
                resolving = false;
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
    keys.forEach((k) => {
      void qc.invalidateQueries({ queryKey: [k] });
      if (["certs", "cert-imported", "certautos"].includes(k)) void qc.invalidateQueries({ queryKey: ["site-certificate-choices"] });
    });
}

/** 停机失败可能仍有 PID，Error 不能直接当作已停止。 */
export function serviceHasProcess(service: ServiceStatus) {
  return service.pids.length > 0 || ["running", "starting", "stopping"].includes(service.state);
}

/** 服务栈入口共用报告，缺失套件不能被全成功提示掩盖。 */
export function toastStackReport(
  t: ReturnType<typeof useT>, report: StackStartReport, action: "start" | "stop",
  retry?: () => Promise<void>
) {
  const details = [
    t("bulk.resultSummary").replace("{ok}", String(report.started.length))
      .replace("{already}", String(report.alreadyRunning.length)).replace("{fail}", String(report.failed.length)),
    ...report.failed.map((f) => `${f.serviceId}: ${f.error.message}`),
    report.skipped.length ? `${t("bulk.unavailable")}: ${report.skipped.join(", ")}` : "",
  ].filter(Boolean).join("\n");
  if (!report.failed.length && !report.skipped.length) {
    toast.success(t(action === "start" ? "stack.startedOk" : "stack.stoppedOk"), { description: details });
    return;
  }
  toast.warning(t("bulk.incomplete"), {
    description: details, duration: 12000,
    action: retry ? { label: t("bulk.retry"), onClick: () => void retry() } : undefined,
  });
  const conflict = action === "start" && report.failed.find((f) => f.error.code === "PORT_IN_USE");
  if (conflict) toastPortConflict(conflict.error, { onResolved: retry });
}

/** 总览、应用菜单和命令面板共享启停流程，包含独立数据库管理台。 */
export function useQuickServiceActions(services: ServiceStatus[], stacks: Stack[]) {
  const t = useT();
  const invalidate = useInvalidate();
  const qc = useQueryClient();
  const adminerQuery = useAdminerStatus();
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [stopReport, setStopReport] = useState<BulkReport | null>(null);
  const [stopError, setStopError] = useState<AppErrorShape | null>(null);
  const [stopTargets, setStopTargets] = useState<string[]>([]);
  const prepareStop = async () => {
    if (busyRef.current) return false;
    busyRef.current = true; setBusy(true);
    setStopReport(null); setStopError(null);
    setStopTargets([]);
    try {
      // 两类进程都读取成功才允许确认；失败不能用空列表伪装为全部停止。
      const [currentServices, consoleStatus] = await Promise.all([
        qc.fetchQuery({ queryKey: ["services"], queryFn: api.listServiceStatus, staleTime: 0, networkMode: "always" }),
        qc.fetchQuery({ queryKey: ["adminer"], queryFn: api.adminerStatus, staleTime: 0, retry: false, networkMode: "always" }),
      ]);
      const ids = currentServices.filter(serviceHasProcess).map((service) => service.id);
      if (consoleStatus) ids.push(api.ADMINER_CONSOLE_ID);
      setStopTargets(ids);
      if (!ids.length) { toast.info(t("bulk.noRunning")); return false; }
      return true;
    } catch (error) {
      toastError(error);
      return false;
    } finally {
      busyRef.current = false; setBusy(false);
    }
  };

  const start = async (stack: Stack | undefined = stacks[0]) => {
    // 固定本次目标；通知稍后重试时不能误用新选择的栈或服务列表。
    const ids = services.filter((s) => ["nginx", "redis", "php", "mysql"].includes(s.id.split("@")[0])).map((s) => s.id);
    const execute = async () => {
      if (busyRef.current) return;
      if (!stack && !ids.length) { toast.error(t("bulk.noServices")); return; }
      busyRef.current = true; setBusy(true);
      const pending = toast.loading(t("dashboard.startingStack"));
      try {
        if (stack) {
          const report = await api.startStack(stack.id);
          toast.dismiss(pending);
          toastStackReport(t, report, "start", execute);
        } else {
          const report = await api.bulkStart(ids);
          toast.dismiss(pending);
          toastStackReport(t, {
            stackId: "", started: report.succeeded, alreadyRunning: report.already,
            failed: report.failed, skipped: [],
          }, "start", execute);
        }
      } catch (error) {
        toast.dismiss(pending);
        if (!toastPortConflict(error, { onResolved: execute })) toastError(error);
      } finally {
        busyRef.current = false; setBusy(false); invalidate("services", "stacks");
      }
    };
    await execute();
  };

  const stop = async (ids = stopTargets) => {
    if (busyRef.current) return null;
    busyRef.current = true; setBusy(true); setStopError(null);
    try {
      const report = await api.stopAllServices(ids);
      setStopReport(report);
      if (!report.failed.length) toast.success(t("bulk.done").replace("{action}", t("bulk.stop")).replace("{n}", String(report.succeeded.length + report.already.length)));
      return report;
    } catch (error) {
      setStopError(normalizeError(error));
      return null;
    } finally {
      busyRef.current = false; setBusy(false); invalidate("services", "stacks", "adminer");
    }
  };
  const serviceAction = async (id: string, action: "start" | "stop" | "restart"): Promise<void> => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true);
    const pending = toast.loading(t("common.loading"));
    try {
      // 固定用户选择的动作，执行和冲突重试前重新读取状态，不能按旧状态反转动作。
      const current = await qc.fetchQuery({ queryKey: ["services"], queryFn: api.listServiceStatus, staleTime: 0, retry: false, networkMode: "always" });
      const service = current.find((item) => item.id === id);
      if (!service) { toast.error(t("svc.notFound")); return; }
      if (service.state === "starting" || service.state === "stopping") {
        toast.info(`${service.label} · ${t(service.state === "starting" ? "state.starting" : "state.stopping")}`);
        return;
      }
      if (action !== "stop" && service.missingRequires.length) {
        toast.warning(t("svc.needDepsHint"));
        return;
      }
      if (action === "start" && service.state !== "running" && serviceHasProcess(service)) {
        toast.warning(t("svc.processStillRunning"));
        return;
      }
      if (action === "stop") await api.stopService(id);
      else if (action === "restart") await api.restartService(id);
      else await api.startService(id);
      toast.success(`${service.label} · ${t(action === "stop" ? "common.stopped" : "common.running")}`);
    } catch (error) {
      if (action === "stop" || !toastPortConflict(error, { onResolved: () => serviceAction(id, action) })) toastError(error);
    } finally {
      toast.dismiss(pending);
      busyRef.current = false; setBusy(false); invalidate("services", "stacks", "sites");
    }
  };
  const pendingTargets = stopReport ? stopReport.failed.map((failure) => failure.serviceId) : stopTargets;
  const stopDescription = t(stopReport?.failed.length ? "bulk.retryStopHint" : "confirm.stopAllDesc")
    .replace("{count}", String(pendingTargets.length))
    + (pendingTargets.includes(api.ADMINER_CONSOLE_ID) ? ` ${t("confirm.stopAllConsoleHint")}` : "");
  const hasStopTargets = services.some(serviceHasProcess) || Boolean(adminerQuery.data) || !adminerQuery.isSuccess;
  return { busy, start, stop, serviceAction, stopReport, stopError, prepareStop, stopDescription, hasStopTargets };
}

/* 端口方案 → 期望端口 */
export function expectedPorts(profile: "safe" | "standard") {
  return profile === "safe"
    ? { http: 8080, https: 8443, mysql: 23306, redis: 26379, apacheHttp: 8180, apacheHttps: 8444, postgres: 25432, mongodb: 28017 }
    : { http: 80, https: 443, mysql: 3306, redis: 6379, apacheHttp: 8080, apacheHttps: 8443, postgres: 5432, mongodb: 27017 };
}

/* 仅展示后端本次加载的入口；端口设置可能尚未应用到运行中的 vhost。 */
export function siteUrl(site: Pick<Site, "accessUrl">) {
  return site.accessUrl ?? "";
}

/* 复制到剪贴板 */
export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast.success("已复制");
    return true;
  } catch {
    toast.error("复制失败，请检查剪贴板权限后重试");
    return false;
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

/** 数据库页与工具箱共享真实管理台状态，换页后仍能打开或停止原进程。 */
function useAdminerStatus() {
  return useQuery({ queryKey: ["adminer"], queryFn: api.adminerStatus, refetchInterval: 5000, retry: false, networkMode: "always" });
}

/** 卡片与列表共用：固定操作和版本，状态重读及端口处理期间禁止重复操作。 */
export function useServiceActions(service: ServiceStatus, disabled = false) {
  const t = useT();
  const qc = useQueryClient();
  const latest = useRef(service);
  latest.current = service;
  const identity = React.useMemo(() => ({ id: service.id, version: service.version }), [service.id, service.version]);
  const latestIdentity = useRef(identity);
  latestIdentity.current = identity;
  const disabledRef = useRef(disabled);
  disabledRef.current = disabled;
  const mounted = useRef(true);
  const operationRef = useRef<object | null>(null);
  const [busy, setBusy] = useState(false);
  type Action = "start" | "stop" | "restart";
  type Conflict = AppErrorShape & { port?: number; pid?: number; holder?: string };
  const [failure, setFailure] = useState<{ action: Action; error: Conflict; conflict?: Conflict } | null>(null);
  useEffect(() => {
    mounted.current = true;
    operationRef.current = null;
    setBusy(false);
    setFailure(null);
    return () => { mounted.current = false; operationRef.current = null; };
  }, [service.id, service.version]);
  const execute = async (action: Action, conflict?: Conflict): Promise<void> => {
    if (!mounted.current || disabledRef.current || operationRef.current) return;
    const { id, version } = service;
    if (latestIdentity.current !== identity || latest.current.id !== id || latest.current.version !== version) {
      toast.error(t("versions.serviceChanged"));
      return;
    }
    const operation = {};
    operationRef.current = operation;
    const current = () => mounted.current && operationRef.current === operation
      && latestIdentity.current === identity && latest.current.id === id && latest.current.version === version && !disabledRef.current;
    setBusy(true);
    setFailure(null);
    let pendingConflict = conflict;
    const read = async () => {
      // 独立读取，不能复用点击前已发出的轮询请求。
      const list = await api.listServiceStatus();
      if (!current()) return;
      const target = list.find((item) => item.id === id);
      if (!target) throw { code: "UNKNOWN_SERVICE", message: t("svc.notFound") };
      if (target.version !== version) throw { code: "SERVICE_TARGET_CHANGED", message: t("versions.serviceChanged") };
      if (["starting", "stopping"].includes(target.state)) throw { code: "SERVICE_BUSY", message: t(`state.${target.state}`) };
      if (target.state === "unknown") throw { code: "SERVICE_STATE_UNKNOWN", message: t("packages.statusUnknown") };
      if (action !== "stop" && target.missingRequires.length) throw { code: "MISSING_DEPENDENCIES", message: t("svc.needDepsHint") };
      if (action === "start" && target.state !== "running" && serviceHasProcess(target)) throw { code: "SERVICE_BUSY", message: t("svc.processStillRunning") };
      return target;
    };
    try {
      let target = await read();
      if (!target || !current()) return;
      if (conflict) {
        const actual = target.lastError;
        if (conflict.port == null || serviceHasProcess(target) || actual?.code !== "PORT_IN_USE"
          || actual.port !== conflict.port || actual.pid !== conflict.pid) {
          throw { code: "PORT_CONFLICT_CHANGED", message: t("svc.portConflictChanged") };
        }
        await api.resolvePortConflict(conflict.port, conflict.pid);
        pendingConflict = undefined;
        if (!current()) return;
        target = await read();
        if (!target || !current()) return;
      }
      if (action === "stop") {
        if (serviceHasProcess(target)) await api.stopService(id, version);
      } else if (action === "restart") await api.restartService(id, version);
      else if (target.state !== "running") await api.startService(id, version);
      if (current()) toast.success(`${target.label} · ${t(action === "stop" ? "common.stopped" : "common.running")}`);
    } catch (error) {
      if (!current()) return;
      const problem = normalizeError(error);
      setFailure({ action, error: problem, conflict: pendingConflict });
      if (action === "stop" || !toastPortConflict(problem, {
        retryLabel: t("svc.freePortAndRetry"), resolve: () => execute(action, problem),
      })) toastError(problem);
    } finally {
      if (operationRef.current === operation) {
        operationRef.current = null;
        if (mounted.current) setBusy(false);
      }
      void qc.invalidateQueries({ queryKey: ["services"] });
      void qc.invalidateQueries({ queryKey: ["stacks"] });
    }
  };
  const conflict = failure
    ? failure.action !== "stop" && failure.error.code === "PORT_IN_USE" ? failure.error : undefined
    : service.lastError;
  return { busy, failure, conflict: conflict?.code === "PORT_IN_USE" && conflict.port != null ? conflict : null,
    toggle: (next: boolean) => execute(next ? "start" : "stop"),
    restart: () => execute("restart"),
    resolveConflict: () => conflict?.code === "PORT_IN_USE" && conflict.port != null
      ? execute(failure?.action ?? "start", conflict) : Promise.resolve(),
    retry: () => failure ? execute(failure.action, failure.conflict) : Promise.resolve(),
  };
}

export function useAdminer(packageId: "adminer" | "phpmyadmin" = "adminer", targetServiceId?: string) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const query = useAdminerStatus();
  const run = async (action: "open" | "stop") => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true);
    try {
      if (action === "stop") {
        await api.adminerStop(); qc.setQueryData(["adminer"], null);
      } else {
        const status = await api.adminerStart(packageId, targetServiceId);
        qc.setQueryData(["adminer"], status);
        await api.openInBrowser(status.url);
      }
    } catch (error) { toastError(error); }
    finally { busyRef.current = false; setBusy(false); void qc.invalidateQueries({ queryKey: ["adminer"] }); }
  };
  return { query, busy, open: () => run("open"), stop: () => run("stop") };
}
