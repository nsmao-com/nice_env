"use client";

import type { DatabaseEngine } from "@nsb/schema";

import * as React from "react";
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import * as api from "./api";
import type { DownloadProgress, VersionCatalog, Site, ServiceStatus, Stack, StackStartReport, BulkReport, BulkTarget } from "@nsb/schema";
import { bulkTarget, mergeBulkReport, sameOptionalVersion } from "./utils";
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

/** 同一 QueryClient 的页面共用请求顺序，IPC Promise 不依赖 cancelQueries 取消。 */
type CatalogRequest = { force: boolean; pending: boolean; promise: Promise<VersionCatalog> };
const catalogRequests = new WeakMap<QueryClient, Map<string, CatalogRequest>>();
const catalogLoadingKey = ["version-catalog-loading"] as const;

function catalogFilters(id: string) {
  return { queryKey: ["version-catalogs"], predicate: (query: { queryKey: readonly unknown[] }) => Array.isArray(query.queryKey[1]) && query.queryKey[1].includes(id) };
}

function previousCatalog(qc: QueryClient, id: string): VersionCatalog | undefined {
  return qc.getQueriesData<VersionCatalog[]>(catalogFilters(id))
    .flatMap(([, catalogs]) => catalogs ?? [])
    .filter((catalog) => catalog.id === id)
    .sort((a, b) => (b.cachedAt ?? 0) - (a.cachedAt ?? 0))[0];
}

function publishVersionCatalog(qc: QueryClient, catalog: VersionCatalog) {
  const current = previousCatalog(qc, catalog.id);
  const newer = current?.cachedAt != null && current.cachedAt > (catalog.cachedAt ?? 0);
  // 最新请求失败时仍展示已知版本，同时保留失败状态与原因。
  const selected = newer ? (catalog.error ? { ...catalog, remote: current.remote, cachedAt: current.cachedAt } : current) : catalog;
  qc.setQueriesData<VersionCatalog[]>(catalogFilters(catalog.id), (previous = []) => {
    const next = new Map(previous.map((item) => [item.id, item]));
    next.set(catalog.id, selected);
    return [...next.values()].sort((a, b) => a.id.localeCompare(b.id));
  });
  return selected;
}

function loadVersionCatalog(qc: QueryClient, id: string, force: boolean): Promise<VersionCatalog> {
  let requests = catalogRequests.get(qc);
  if (!requests) { requests = new Map(); catalogRequests.set(qc, requests); }
  const current = requests.get(id);
  if (current?.pending && (current.force || !force)) return current.promise;
  const active = requests;
  const request: CatalogRequest = {
    force,
    pending: true,
    promise: Promise.resolve().then(async () => {
      try {
        const catalog = await api.versionCatalog(id, force);
        const latest = active.get(id);
        if (latest && latest !== request) return latest.promise;
        return publishVersionCatalog(qc, catalog);
      } catch (error) {
        const latest = active.get(id);
        if (latest && latest !== request) return latest.promise;
        publishVersionCatalog(qc, {
          ...(previousCatalog(qc, id) ?? { id, remote: [] }), online: false, error: normalizeError(error).message,
        });
        throw error;
      } finally {
        request.pending = false;
        if (active.get(id) === request) {
          qc.setQueryData<Record<string, boolean>>(catalogLoadingKey, (loading = {}) => ({ ...loading, [id]: false }));
        }
      }
    }),
  };
  active.set(id, request);
  qc.setQueryData<Record<string, boolean>>(catalogLoadingKey, (loading = {}) => ({ ...loading, [id]: true }));
  return request.promise;
}

/** 项目 LTS 刷新与套件菜单使用同一入口，不创建旧的单项缓存键。 */
export async function refreshVersionCatalog(qc: QueryClient, id: string) {
  return loadVersionCatalog(qc, id, true);
}

/**
 * 远程版本目录：返回 id → 该包从上游枚举到的完整版本列表。
 * 后端带 6 小时缓存，这里 staleTime 设长一些避免重复请求；
 * 「刷新版本」用 refresh() 强制绕过缓存。
 */
export function useVersionCatalogs(packageIds: string[]) {
  const qc = useQueryClient();
  const ids = React.useMemo(() => {
    const priority = new Map(["nginx", "php", "mysql", "redis", "node", "python", "go"].map((id, index) => [id, index]));
    return [...new Set(packageIds)].sort((a, b) => (priority.get(a) ?? 100) - (priority.get(b) ?? 100) || a.localeCompare(b));
  }, [packageIds.join("\u0000")]);
  const idKey = ids.join("\u0000");
  // 目录不能再用一次批量 IPC 阻塞几十个上游请求；每个套件独立读取，先显示
  // 清单/缓存版本，再把远程结果按有限并发写回同一个查询缓存。
  const queryKey = React.useMemo(() => ["version-catalogs", ids] as const, [idKey]);
  const query = useQuery({
    queryKey,
    queryFn: async () => [],
    staleTime: Infinity,
    retry: false,
    enabled: false,
    initialData: [] as VersionCatalog[],
  });
  const { data: loading = {} } = useQuery<Record<string, boolean>>({
    queryKey: catalogLoadingKey,
    queryFn: async () => ({}),
    enabled: false,
    initialData: {},
  });
  const [loadingIds, setLoadingIds] = React.useState<Set<string>>(() => new Set());
  const loadGeneration = React.useRef(0);
  const loadCatalogs = React.useCallback(async (requestedIds: string[], force: boolean) => {
    const queue = [...new Set(requestedIds)];
    const generation = ++loadGeneration.current;
    setLoadingIds(new Set(queue));
    let cursor = 0;
    const results: VersionCatalog[] = [];
    const worker = async () => {
      while (generation === loadGeneration.current) {
        const id = queue[cursor++];
        if (!id) return;
        const pending = loadVersionCatalog(qc, id, force);
        setLoadingIds((previous) => {
          const next = new Set(previous);
          next.delete(id);
          return next;
        });
        try {
          results.push(await pending);
        } catch (error) {
          results.push({
            ...(previousCatalog(qc, id) ?? { id, remote: [] }),
            online: false,
            error: normalizeError(error).message,
          });
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(6, queue.length) }, () => worker()));
    return results;
  }, [qc]);

  React.useEffect(() => {
    if (ids.length === 0) {
      loadGeneration.current += 1;
      setLoadingIds(new Set());
      return;
    }
    void loadCatalogs(ids, false);
    return () => {
      loadGeneration.current += 1;
    };
  }, [idKey, ids, loadCatalogs]);

  const catalogs = new Map((query.data ?? []).map((catalog) => [catalog.id, catalog]));
  const byId = new Map<string, VersionCatalog & { loading: boolean }>();
  ids.forEach((id) => {
    const catalog = catalogs.get(id);
    byId.set(id, {
      id, remote: [], online: false,
      ...catalog,
      loading: loadingIds.has(id) || loading[id] === true,
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
    return loadCatalogs(ids, true);
  }, [ids, loadCatalogs]);
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

type ServiceAction = "start" | "stop" | "restart";
type ServiceConflict = AppErrorShape & { port?: number; pid?: number; holder?: string };
type ServiceTarget = Pick<ServiceStatus, "id" | "version" | "label">;

/** 卡片、列表和命令面板复用版本、状态和冲突核对，入口仅负责进度及反馈。 */
async function performServiceAction(
  { id, version }: ServiceTarget, action: ServiceAction, t: ReturnType<typeof useT>,
  current: () => boolean, conflict?: ServiceConflict, onPortResolved?: () => void,
): Promise<ServiceStatus | undefined> {
  const read = async () => {
    // 独立读取，不能复用点击前已发出的轮询请求。
    const list = await api.listServiceStatus();
    if (!current()) return;
    const target = list.find((item) => item.id === id);
    if (!target) throw { code: "UNKNOWN_SERVICE", message: t("svc.notFound") };
    if (!sameOptionalVersion(target.version, version)) throw { code: "SERVICE_TARGET_CHANGED", message: t("versions.serviceChanged") };
    if (["starting", "stopping"].includes(target.state)) throw { code: "SERVICE_BUSY", message: t(`state.${target.state}`) };
    if (target.state === "unknown") throw { code: "SERVICE_STATE_UNKNOWN", message: t("packages.statusUnknown") };
    if (action !== "stop" && target.missingRequires.length) throw { code: "MISSING_DEPENDENCIES", message: t("svc.needDepsHint") };
    if (action === "start" && target.state !== "running" && serviceHasProcess(target)) throw { code: "SERVICE_BUSY", message: t("svc.processStillRunning") };
    return target;
  };
  if (!current()) return;
  let target = await read();
  if (!target || !current()) return;
  if (conflict) {
    const actual = target.lastError;
    if (conflict.port == null || serviceHasProcess(target) || actual?.code !== "PORT_IN_USE"
      || actual.port !== conflict.port || actual.pid !== conflict.pid) {
      throw { code: "PORT_CONFLICT_CHANGED", message: t("svc.portConflictChanged") };
    }
    await api.resolvePortConflict(conflict.port, conflict.pid);
    onPortResolved?.();
    if (!current()) return;
    target = await read();
    if (!target || !current()) return;
  }
  if (action === "stop") {
    if (serviceHasProcess(target)) await api.stopService(id, version);
  } else if (action === "restart") await api.restartService(id, version);
  else if (target.state !== "running") await api.startService(id, version);
  return target;
}

/** 服务栈入口共用报告，缺失套件不能被全成功提示掩盖。 */
export function toastStackReport(
  t: ReturnType<typeof useT>, report: StackStartReport, action: "start" | "stop",
  showDetails?: () => void
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
    action: showDetails ? { label: t("stack.viewResult"), onClick: showDetails } : undefined,
  });
}

export type StackActionResult = {
  sequence: number;
  name: string;
  action: "start" | "stop";
  report: StackStartReport | null;
  error: AppErrorShape | null;
};
type StackActionState = { sequence: number; busyId: string | null; results: Record<string, StackActionResult> };
const STACK_ACTION_KEY = ["stack-actions"];
const emptyStackActions = (): StackActionState => ({ sequence: 0, busyId: null, results: {} });

/** 会话内共享结果：快捷入口完成后换到服务栈页面，仍能查看并重试原失败项。 */
export function useStackActions() {
  const t = useT();
  const qc = useQueryClient();
  const router = useRouter();
  const invalidate = useInvalidate();
  const { data } = useQuery<StackActionState>({
    queryKey: STACK_ACTION_KEY, queryFn: emptyStackActions, initialData: emptyStackActions,
    enabled: false, staleTime: Infinity, gcTime: Infinity,
  });
  const current = () => qc.getQueryData<StackActionState>(STACK_ACTION_KEY) ?? emptyStackActions();
  const execute = async (id: string, name: string, action: "start" | "stop", previous?: StackActionResult) => {
    const state = current();
    if (state.busyId !== null || (previous && state.results[id]?.sequence !== previous.sequence)) return;
    const original = previous?.report;
    if (previous && (!original?.revision || !original.failed.length)) return;
    const selected = original?.failed.map((failure) => failure.serviceId);
    const sequence = state.sequence + 1;
    const result: StackActionResult = { sequence, name, action, report: original ?? null, error: null };
    qc.setQueryData<StackActionState>(STACK_ACTION_KEY, { sequence, busyId: id, results: { ...state.results, [id]: { ...result } } });
    const pending = toast.loading(`${name} · ${t(`common.${action}`)}`);
    try {
      const report = original && selected
        ? await api.retryStack(id, action, { revision: original.revision, serviceIds: selected })
        : await (action === "start" ? api.startStack(id) : api.stopStack(id));
      if (original && selected) {
        // 合并失败项的新结果，保留先前成功项和缺失项；不会再次操作成功服务。
        result.report = { ...report, order: original.order,
          started: [...original.started.filter((sid) => !selected.includes(sid)), ...report.started],
          alreadyRunning: [...original.alreadyRunning.filter((sid) => !selected.includes(sid)), ...report.alreadyRunning],
          failed: [...original.failed.filter((failure) => !selected.includes(failure.serviceId)), ...report.failed],
          skipped: [...new Set([...original.skipped, ...report.skipped])],
        };
      } else result.report = report;
      toastStackReport(t, result.report, action, () => router.push("/stacks"));
    } catch (error) {
      result.error = normalizeError(error);
      toast.error(result.error.message, { description: result.error.hint,
        action: { label: t("stack.viewResult"), onClick: () => router.push("/stacks") } });
    } finally {
      qc.setQueryData<StackActionState>(STACK_ACTION_KEY, (latest) => latest?.results[id]?.sequence === sequence
        ? { ...latest, busyId: null, results: { ...latest.results, [id]: { ...result } } } : latest);
      toast.dismiss(pending);
      invalidate("services", "stacks");
    }
  };
  return {
    results: data.results, busyId: data.busyId, isBusy: () => current().busyId !== null,
    run: (stack: Stack, action: "start" | "stop") => execute(stack.id, stack.name, action),
    retry: (id: string, result: StackActionResult) => execute(id, result.name, result.action, result),
    dismiss: (id: string) => {
      const state = current();
      if (state.busyId !== null) return;
      const results = { ...state.results }; delete results[id];
      qc.setQueryData(STACK_ACTION_KEY, { ...state, results });
    },
  };
}

/** 总览、应用菜单和命令面板共享启停流程，包含独立数据库管理台。 */
export function useQuickServiceActions(services: ServiceStatus[], stacks: Stack[]) {
  const t = useT();
  const invalidate = useInvalidate();
  const stackActions = useStackActions();
  const adminerQuery = useAdminerStatus();
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [stopReport, setStopReport] = useState<BulkReport | null>(null);
  const [stopError, setStopError] = useState<AppErrorShape | null>(null);
  const [stopTargets, setStopTargets] = useState<BulkTarget[]>([]);
  const mounted = useRef(true);
  const serviceRequest = useRef<object | null>(null);
  const serviceIdentities = useRef(new Map<string, { version?: string }>());
  const present = new Set(services.map((service) => service.id));
  for (const service of services) {
    const previous = serviceIdentities.current.get(service.id);
    if (!previous || !sameOptionalVersion(previous.version, service.version)) serviceIdentities.current.set(service.id, { version: service.version });
  }
  for (const id of serviceIdentities.current.keys()) if (!present.has(id)) serviceIdentities.current.delete(id);
  const [serviceFailure, setServiceFailure] = useState<{
    service: ServiceTarget; action: ServiceAction; error: ServiceConflict; identity: object | undefined;
    retry: () => Promise<void>; resolve: (() => Promise<void>) | null;
  } | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; serviceRequest.current = null; };
  }, []);
  const prepareStop = async () => {
    if (!mounted.current || busyRef.current || stackActions.isBusy()) return false;
    busyRef.current = true; setBusy(true);
    setStopReport(null); setStopError(null);
    setStopTargets([]);
    try {
      // 两类进程都读取成功才允许确认；失败不能用空列表伪装为全部停止。
      const [currentServices, consoleStatus] = await Promise.all([
        api.listServiceStatus(),
        api.adminerStatus(),
      ]);
      if (!mounted.current) return false;
      const targets = currentServices.filter(serviceHasProcess).map(bulkTarget);
      if (consoleStatus) {
        if (!consoleStatus.revision) throw { code: "SERVICE_TARGET_CHANGED", message: t("bulk.consoleChanged") };
        targets.push({ id: api.ADMINER_CONSOLE_ID, version: null, revision: consoleStatus.revision,
          label: `${t("tools.adminer.title")} · ${consoleStatus.packageId ?? "adminer"} ${consoleStatus.adminerVersion} · PHP ${consoleStatus.phpVersion}` });
      }
      setStopTargets(targets);
      if (!targets.length) { toast.info(t("bulk.noRunning")); return false; }
      return true;
    } catch (error) {
      toastError(error);
      return false;
    } finally {
      busyRef.current = false; if (mounted.current) setBusy(false);
    }
  };

  const start = async (stack: Stack | undefined = stacks[0]) => {
    if (busyRef.current || stackActions.isBusy()) return;
    if (stack) { await stackActions.run(stack, "start"); return; }
    // 尚无服务栈时保留首次使用的快速启动入口。
    const targets = services.filter((s) => ["nginx", "redis", "php", "mysql"].includes(s.id.split("@")[0])).map(bulkTarget);
    if (!targets.length) { toast.error(t("bulk.noServices")); return; }
    busyRef.current = true; setBusy(true);
    const pending = toast.loading(t("dashboard.startingStack"));
    try {
      const report = await api.bulkStart(targets);
      toast.dismiss(pending);
      toastStackReport(t, {
        stackId: "", revision: "", order: report.order, started: report.succeeded, alreadyRunning: report.already,
        failed: report.failed, skipped: [],
      }, "start");
    } catch (error) {
      toast.dismiss(pending);
      toastError(error);
    } finally {
      busyRef.current = false; setBusy(false); invalidate("services", "stacks");
    }
  };

  const stop = async (ids = stopReport ? stopReport.failed.map((failure) => failure.serviceId) : stopTargets.map((target) => target.id)) => {
    if (!mounted.current || busyRef.current || stackActions.isBusy()) return null;
    const targets = stopTargets.filter((target) => ids.includes(target.id));
    if (!targets.length || ids.some((id) => !targets.some((target) => target.id === id))) return null;
    busyRef.current = true; setBusy(true); setStopError(null);
    try {
      const report = await api.stopAllServices(targets);
      if (mounted.current) setStopReport((previous) => previous ? mergeBulkReport(previous, report) : report);
      if (!report.failed.length) toast.success(t("bulk.done").replace("{action}", t("bulk.stop")).replace("{n}", String(report.succeeded.length + report.already.length)));
      return report;
    } catch (error) {
      if (mounted.current) setStopError(normalizeError(error));
      return null;
    } finally {
      busyRef.current = false; if (mounted.current) setBusy(false); invalidate("services", "stacks", "adminer");
    }
  };
  const serviceAction = async (selected: ServiceTarget, action: ServiceAction): Promise<void> => {
    if (!mounted.current || busyRef.current || stackActions.isBusy()) return;
    const service = { id: selected.id, version: selected.version, label: selected.label };
    const identity = serviceIdentities.current.get(service.id);
    const request = {};
    serviceRequest.current = request;
    const active = () => mounted.current && serviceRequest.current === request;
    const current = () => active() && identity !== undefined
      && serviceIdentities.current.get(service.id) === identity && sameOptionalVersion(identity.version, service.version);
    const execute = async (conflict?: ServiceConflict): Promise<void> => {
      if (!active() || busyRef.current || stackActions.isBusy()) return;
      busyRef.current = true; setBusy(true); setServiceFailure(null);
      const pending = toast.loading(`${service.label}${service.version ? ` ${service.version}` : ""} · ${t(`common.${action}`)}`);
      let pendingConflict = conflict;
      try {
        const result = await performServiceAction(service, action, t, current, conflict, () => { pendingConflict = undefined; });
        if (!active()) return;
        if (!current()) throw { code: "SERVICE_TARGET_CHANGED", message: t("versions.serviceChanged") };
        if (result) {
          toast.success(`${result.label} · ${t(action === "stop" ? "common.stopped" : "common.running")}`);
          serviceRequest.current = null;
        }
      } catch (error) {
        if (!active()) return;
        const problem: ServiceConflict = normalizeError(error);
        const retry = () => execute(pendingConflict);
        const resolve = action !== "stop" && problem.code === "PORT_IN_USE" && problem.port != null ? () => execute(problem) : null;
        setServiceFailure({ service, action, error: problem, identity, retry, resolve });
        if (!resolve || !toastPortConflict(problem, { retryLabel: t("svc.freePortAndRetry"), resolve })) {
          toast.error(problem.message, { description: problem.hint, action: { label: `${t("bulk.retry")} · ${t(`common.${action}`)}`, onClick: retry } });
        }
      } finally {
        toast.dismiss(pending);
        busyRef.current = false;
        if (mounted.current) setBusy(false);
        invalidate("services", "stacks", "sites");
      }
    };
    await execute();
  };
  const dismissServiceFailure = () => {
    if (busyRef.current) return;
    serviceRequest.current = null;
    setServiceFailure(null);
  };
  const serviceTargetChanged = Boolean(serviceFailure && (!serviceFailure.identity || serviceFailure.identity !== serviceIdentities.current.get(serviceFailure.service.id)
    || !sameOptionalVersion(serviceFailure.service.version, serviceIdentities.current.get(serviceFailure.service.id)?.version)));
  const pendingTargets = stopReport ? stopReport.failed.map((failure) => failure.serviceId) : stopTargets.map((target) => target.id);
  const stopDescription = t(stopReport?.failed.length ? "bulk.retryStopHint" : "confirm.stopAllDesc")
    .replace("{count}", String(pendingTargets.length))
    + (pendingTargets.includes(api.ADMINER_CONSOLE_ID) ? ` ${t("confirm.stopAllConsoleHint")}` : "");
  const hasStopTargets = services.some(serviceHasProcess) || Boolean(adminerQuery.data) || !adminerQuery.isSuccess;
  return { busy: busy || stackActions.busyId !== null, start, stop, serviceAction, serviceFailure, serviceTargetChanged, dismissServiceFailure,
    stopReport, stopError, stopTargets, prepareStop, stopDescription, hasStopTargets };
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
  const [failure, setFailure] = useState<{ action: ServiceAction; error: ServiceConflict; conflict?: ServiceConflict } | null>(null);
  useEffect(() => {
    mounted.current = true;
    operationRef.current = null;
    setBusy(false);
    setFailure(null);
    return () => { mounted.current = false; operationRef.current = null; };
  }, [service.id, service.version]);
  const execute = async (action: ServiceAction, conflict?: ServiceConflict): Promise<void> => {
    if (!mounted.current || disabledRef.current || operationRef.current) return;
    const { id, version } = service;
    if (latestIdentity.current !== identity || latest.current.id !== id || !sameOptionalVersion(latest.current.version, version)) {
      toast.error(t("versions.serviceChanged"));
      return;
    }
    const operation = {};
    operationRef.current = operation;
    const current = () => mounted.current && operationRef.current === operation
      && latestIdentity.current === identity && latest.current.id === id && sameOptionalVersion(latest.current.version, version) && !disabledRef.current;
    setBusy(true);
    setFailure(null);
    let pendingConflict = conflict;
    try {
      const target = await performServiceAction(service, action, t, current, conflict, () => { pendingConflict = undefined; });
      if (!target || !current()) return;
      toast.success(`${target.label} · ${t(action === "stop" ? "common.stopped" : "common.running")}`);
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
