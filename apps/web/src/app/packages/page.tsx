"use client";

import * as React from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { motion, AnimatePresence } from "motion/react";
import {
  Server,
  Boxes,
  Database,
  Zap,
  Wrench,
  Trash2,
  Download,
  Loader2,
  Search,
  Check,
  LayoutGrid,
  Play,
  Square,
  Power,
  Sparkles,
  Bot,
  Container,
  Waypoints,
  Network,
  Mail,
  Globe2,
  UploadCloud,
  FolderArchive,
  HardDrive,
} from "lucide-react";
import type { PackageView, PackageCategory, ServiceStatus, BulkReport } from "@nsb/schema";
import { PACKAGE_CATEGORY_ORDER } from "@nsb/schema";
import { cn, fmtBytes, fmtSpeed, isPlatformCompatible } from "@/lib/utils";
import { useUI, useT } from "@/lib/store";
import { usePackages, useServices, useVersionCatalogs, serviceHasProcess } from "@/lib/hooks";
import { normalizeError, type AppErrorShape } from "@/lib/backend";
import { useInstallTasks, activeProgressFor } from "@/lib/install-tasks";
import * as api from "@/lib/api";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Tooltip, TooltipTrigger, TooltipContent } from "@/components/ui/tooltip";
import { RingProgress } from "@/components/shared/ring-progress";
import { VersionPicker, type VersionItem } from "@/components/shared/version-picker";
import { PhpExtensionsDialog, PhpExtBadge } from "@/components/shared/php-extensions";
import { ConfirmDialog } from "@/components/shared/misc";
import { BulkResult } from "@/components/shared/bulk-actions";
import { InstallDialog, type InstallTarget } from "@/components/shared/install-dialog";
import { PathEnvToggle } from "@/components/shared/path-env-toggle";
import { ServiceIcon } from "@/components/shared/service-icon";
import { PageHeader } from "@/components/layout/app-shell";
import { cmpVersionDesc, isPrerelease } from "@/lib/utils";

const CATEGORY_ICONS: Record<string, React.ComponentType<{ className?: string; strokeWidth?: number }>> = {
  "web-server": Server,
  runtime: Boxes,
  database: Database,
  cache: Zap,
  tool: Wrench,
  "ai-coding": Bot,
  ai: Sparkles,
  container: Container,
  tunnel: Waypoints,
  "service-mesh": Network,
  mail: Mail,
  dns: Globe2,
  ftp: UploadCloud,
  search: Search,
  "object-storage": HardDrive,
  other: FolderArchive,
};

/** 分类 → 大类归属（大类+小类两级筛选）；清单新出现的未知类别自动落进「其他」 */
const CATEGORY_GROUP: Record<string, string> = {
  "web-server": "web",
  "service-mesh": "web",
  runtime: "runtime",
  database: "data",
  cache: "data",
  search: "data",
  "object-storage": "data",
  dns: "net",
  ftp: "net",
  mail: "net",
  tunnel: "net",
  ai: "ai",
  "ai-coding": "ai",
  tool: "tools",
  container: "tools",
  other: "other",
};

/** 大类展示顺序；只有清单里真正出现的才会出现 */
const CATEGORY_GROUPS = [
  { id: "web", icon: Server },
  { id: "runtime", icon: Boxes },
  { id: "data", icon: Database },
  { id: "net", icon: Waypoints },
  { id: "ai", icon: Sparkles },
  { id: "tools", icon: Wrench },
  { id: "other", icon: FolderArchive },
] as const;

/**
 * 服务语义完全由清单声明驱动：
 *  - 包带有 `run` 描述 = 守护进程型（可启停）
 *  - `run.singleInstance` 决定「切换使用版本」还是「多实例并行」
 * 这样新增服务只需改清单，前端不用改代码。
 */
function isService(p: PackageView) {
  return !!p.run;
}

function serviceIdOf(p: Pick<PackageView, "id" | "version" | "run">): string | null {
  if (!p.run) return null;
  return p.run.singleInstance === false ? `${p.id}@${p.version}` : p.id;
}

interface PackageGroup {
  id: string;
  displayName: string;
  description: string;
  category: string;
  defaultPort?: number;
  /** 清单声明了 run → 可启停服务 */
  isService: boolean;
  /** 清单声明 run.singleInstance=false → 多实例并行（如 PHP/MySQL） */
  multiInstance: boolean;
  versions: {
    version: string;
    sizeBytes: number;
    installed: boolean;
    active: boolean;
    serviceId: string | null;
    /** 该清单条目不支持当前平台（下载前会被后端拦截） */
    incompatible?: boolean;
  }[];
}

function groupPackages(packages: PackageView[]): PackageGroup[] {
  const map = new Map<string, PackageGroup>();
  for (const p of packages) {
    let g = map.get(p.id);
    if (!g) {
      g = {
        id: p.id,
        // 分组名称不绑定内置清单第一条的旧版本；固定大版本的套件仍保留名称。
        displayName: ["php", "node", "python", "go", "mysql", "mongodb", "postgresql", "composer", "adminer", "mariadb", "gradle", "tomcat", "erlang", "bun", "k6", "neo4j"].includes(p.id)
          ? p.displayName.replace(/\s+\d[\d.]*\s*(?:LTS)?$/, "")
          : p.displayName,
        description: ["php", "node", "python"].includes(p.id)
          ? p.description.replace(/^(PHP|Node\.js|Python)\s+[\d.]+(?: LTS)?/, "$1")
          : p.description,
        category: p.category,
        defaultPort: p.defaultPort,
        isService: isService(p),
        multiInstance: p.run?.singleInstance === false,
        versions: [],
      };
      map.set(p.id, g);
    }
    if (!g.description && p.description) g.description = p.description;
    g.versions.push({
      version: p.version,
      sizeBytes: p.sizeBytes,
      installed: !!p.install,
      active: !!p.active,
      serviceId: serviceIdOf(p),
      incompatible: !isPlatformCompatible(p.os as string[] | undefined, p.arch as string[] | undefined),
    });
  }
  for (const g of map.values()) {
    g.versions.sort((a, b) => cmpVersionDesc(a.version, b.version));
  }
  return [...map.values()];
}

export default function PackagesPage() {
  const t = useT();
  const [uninstalling, setUninstalling] = React.useState(false);
  const uninstallRef = React.useRef(false);
  const uninstallOpener = React.useRef<HTMLButtonElement | null>(null);
  const [uninstallError, setUninstallError] = React.useState<AppErrorShape | null>(null);
  const [uninstallPathPending, setUninstallPathPending] = React.useState(false);
  const packageQuery = usePackages();
  const serviceQuery = useServices(2000);
  const packages = packageQuery.data;
  const services = serviceQuery.data;
  const queryClient = useQueryClient();
  const refreshState = (includeCatalogs = false) => Promise.all(
    ["packages", "services", "pathenv", "stacks", "databases", "db-users", ...(includeCatalogs ? ["version-catalogs"] : [])].map(
      (key) => queryClient.invalidateQueries({ queryKey: [key] })
    )
  );
  const statusKnown = serviceQuery.dataUpdatedAt > 0 && !serviceQuery.error;
  const dataReady = packageQuery.dataUpdatedAt > 0 && !packageQuery.error && statusKnown;
  const [bulkTarget, setBulkTarget] = React.useState<{ action: "start" | "stop"; ids: string[] } | null>(null);
  const [bulkBusy, setBulkBusy] = React.useState(false);
  const bulkRef = React.useRef(false);
  const [bulkReport, setBulkReport] = React.useState<BulkReport | null>(null);
  const [bulkError, setBulkError] = React.useState<AppErrorShape | null>(null);
  const installTasks = useInstallTasks((s) => s.tasks);
  const {
    byId: catalogById,
    refresh: refreshCatalogs,
  } = useVersionCatalogs(packages.map((p) => p.id));
  const [query, setQuery] = React.useState("");
  const [uninstallTarget, setUninstallTarget] = React.useState<{ id: string; version: string; name: string } | null>(null);
  const [installTarget, setInstallTarget] = React.useState<InstallTarget | null>(null);
  const uninstallInstalling = !!uninstallTarget && Object.values(installTasks).some((task) =>
    task.status === "running" && task.id === uninstallTarget.id
    && (!task.version || task.version === uninstallTarget.version)
  );
  const actionsDisabled = !dataReady || bulkBusy || uninstalling;

  const groups = React.useMemo(() => groupPackages(packages), [packages]);
  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return groups;
    return groups.filter(
      (g) =>
        g.displayName.toLowerCase().includes(q) ||
        g.id.toLowerCase().includes(q) ||
        g.description.toLowerCase().includes(q)
    );
  }, [groups, query]);

  const runningServices = React.useMemo(
    () => new Set(services.filter((s) => statusKnown && s.state === "running").map((s) => s.id)),
    [services, statusKnown]
  );

  /** 大类（第一行胶囊）→ 小类（选中大类后出现的第二行胶囊）。
      归属见 CATEGORY_GROUP；某个大类下只剩一个分类时不再显示小类行。 */
  const catGroups = React.useMemo(() => {
    type CatItem = { value: string; label: string; icon: React.ComponentType<{ className?: string; strokeWidth?: number }> };
    const used = new Set(groups.map((g) => g.category));
    const byGroup = new Map<string, CatItem[]>();
    const push = (gid: string, item: CatItem) => {
      const arr = byGroup.get(gid) ?? [];
      arr.push(item);
      byGroup.set(gid, arr);
    };
    // 先按 schema 的固定顺序归位，保证小类行顺序稳定
    for (const c of PACKAGE_CATEGORY_ORDER) {
      if (!used.has(c)) continue;
      push(CATEGORY_GROUP[c] ?? "other", {
        value: c,
        label: t(`packages.cat.${c}` as never),
        icon: CATEGORY_ICONS[c] ?? Boxes,
      });
    }
    // 清单里新出现的未知类别 → 归入「其他」（前向兼容远程更新的清单）
    for (const c of used) {
      if (PACKAGE_CATEGORY_ORDER.includes(c as PackageCategory)) continue;
      push("other", { value: c, label: c, icon: Boxes });
    }
    return CATEGORY_GROUPS.filter((g) => byGroup.has(g.id)).map((g) => ({
      id: g.id,
      icon: g.icon,
      subs: byGroup.get(g.id) ?? [],
    }));
  }, [groups, t]);

  const catCount = (v: string) => filtered.filter((g) => g.category === v).length;
  const groupCount = (gid: string) => {
    const grp = catGroups.find((x) => x.id === gid);
    return grp ? filtered.filter((g) => grp.subs.some((s) => s.value === g.category)).length : 0;
  };

  const openBulk = (action: "start" | "stop") => {
    if (!dataReady || bulkRef.current || uninstallRef.current) return;
    const installedIds = new Set(groups
      .filter((g) => g.isService)
      .flatMap((g) => g.versions.filter((v) => v.installed).map((v) => v.serviceId))
      .filter((id): id is string => !!id));
    const ids = action === "start"
      ? [...installedIds].filter((id) => !runningServices.has(id))
      : services.filter((s) => installedIds.has(s.id) && serviceHasProcess(s)).map((s) => s.id);
    if (ids.length === 0) {
      toast.info(t(action === "start" ? "packages.nothingToStart" : "packages.nothingToStop"));
      return;
    }
    setBulkReport(null);
    setBulkError(null);
    setBulkTarget({ action, ids });
  };

  const runBulk = async () => {
    if (!bulkTarget || bulkRef.current || !dataReady) return;
    const ids = bulkReport ? bulkReport.failed.map((item) => item.serviceId) : bulkTarget.ids;
    if (ids.length === 0) return;
    bulkRef.current = true;
    setBulkBusy(true);
    setBulkError(null);
    try {
      const result = await (bulkTarget.action === "start" ? api.bulkStart(ids) : api.bulkStop(ids));
      setBulkReport((previous) => previous ? {
        ...result,
        order: previous.order,
        succeeded: [...new Set([...previous.succeeded, ...result.succeeded])],
        already: [...new Set([...previous.already, ...result.already])],
      } : result);
      if (result.failed.length === 0) {
        toast.success(t("bulk.done").replace("{action}", t(bulkTarget.action === "start" ? "bulk.start" : "bulk.stop"))
          .replace("{n}", String(bulkTarget.ids.length)));
        setBulkTarget(null);
      }
    } catch (error) {
      setBulkError(normalizeError(error));
    } finally {
      await refreshState();
      bulkRef.current = false;
      setBulkBusy(false);
    }
  };

  const openUninstall = (group: PackageGroup, version: string, trigger: HTMLButtonElement | null) => {
    if (actionsDisabled) return;
    uninstallOpener.current = trigger;
    setUninstallError(null);
    setUninstallPathPending(false);
    setUninstallTarget({ id: group.id, version, name: group.displayName });
  };

  const uninstall = async () => {
    if (!uninstallTarget || uninstallRef.current || (!uninstallPathPending && (!dataReady || uninstallInstalling))) return;
    uninstallRef.current = true;
    setUninstalling(true);
    setUninstallError(null);
    try {
      if (uninstallPathPending) {
        const result = await api.pathenvReapply();
        queryClient.setQueryData(["pathenv"], result);
        toast.success(t("packages.pathCleaned"));
      } else {
        await api.uninstallPackage(`${uninstallTarget.id}@${uninstallTarget.version}`);
        toast.success(`${uninstallTarget.name} ${uninstallTarget.version} ${t("packages.uninstalled")}`);
      }
      setUninstallTarget(null);
    } catch (error) {
      const normalized = normalizeError(error);
      if (normalized.code === "UNINSTALL_PATH_SYNC_FAILED") setUninstallPathPending(true);
      setUninstallError(normalized);
    } finally {
      await refreshState(true);
      uninstallRef.current = false;
      setUninstalling(false);
    }
  };

  const readStatus = !dataReady && (
    <div role={packageQuery.error || serviceQuery.error ? "alert" : "status"} className="mb-4 flex flex-col items-start gap-3 rounded-xl border border-border bg-card p-3 text-xs leading-relaxed [overflow-wrap:anywhere] sm:flex-row sm:items-center">
      <div className="min-w-0 flex-1 space-y-1">
        {packageQuery.error && <p className="text-error">{t("packages.readFailed")}</p>}
        {serviceQuery.error && <p className="text-error">{t("packages.servicesReadFailed")}</p>}
        {!packageQuery.error && !serviceQuery.error && <p className="text-muted">{t("common.loading")}</p>}
      </div>
      {(packageQuery.error || serviceQuery.error) && (
        <Button size="sm" variant="secondary" disabled={packageQuery.isFetching || serviceQuery.isFetching || bulkBusy || uninstalling}
          onClick={() => void Promise.all([packageQuery.refetch(), serviceQuery.refetch()])}>
          {t("packages.reload")}
        </Button>
      )}
    </div>
  );

  return (
    <div className="pb-8">
      <PageHeader
        title={t("packages.title")}
        subtitle={t("packages.subtitle")}
        actions={
          <div className="flex max-w-full flex-wrap items-center gap-2">
            <div className="relative w-full sm:w-auto">
              <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-faint" />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("packages.search")}
                aria-label={t("packages.search")}
                className="h-8 w-full rounded-lg border border-border bg-card pl-8 pr-3 text-[13px] outline-none transition-colors placeholder:text-faint focus:border-primary sm:w-52"
              />
            </div>
            <Button variant="outline" size="sm" disabled={actionsDisabled} onClick={() => openBulk("start")}>
              <Play className="h-3.5 w-3.5" /> {t("packages.startAll")}
            </Button>
            <Button variant="outline" size="sm" disabled={actionsDisabled} onClick={() => openBulk("stop")}>
              <Square className="h-3 w-3" /> {t("packages.stopAll")}
            </Button>
          </div>
        }
      />

      {readStatus}

      {packageQuery.dataUpdatedAt > 0 && <Tabs defaultValue="all">
        {/* 第一行：大类（全部 + 分组）；小类在选中大类后出现在第二行 */}
        <TabsList className="max-w-full flex-wrap justify-start rounded-2xl">
          <TabsTrigger value="all">
            <LayoutGrid className="h-3.5 w-3.5" />
            {t("packages.cat.all")}
            <span className="text-[10.5px] tabular text-faint">{filtered.length}</span>
          </TabsTrigger>
          {catGroups.map((g) => (
            <TabsTrigger key={g.id} value={g.id}>
              <g.icon className="h-3.5 w-3.5" />
              {t(`packages.group.${g.id}` as never)}
              <span className="text-[10.5px] tabular text-faint">{groupCount(g.id)}</span>
            </TabsTrigger>
          ))}
        </TabsList>

        <TabsContent value="all" className="mt-4">
          <PackageRows
            list={filtered}
            services={services}
            runningServices={runningServices}
            catalogById={catalogById}
            onRefresh={refreshCatalogs}
            onUninstallTarget={openUninstall}
            onInstall={(target) => setInstallTarget(target)}
            empty={t("packages.noMatches")}
            disabled={actionsDisabled}
            statusKnown={statusKnown}
          />
        </TabsContent>

        {catGroups.map((g) => {
          const rows = filtered.filter((x) => g.subs.some((s) => s.value === x.category));
          return (
            <TabsContent key={g.id} value={g.id} className="mt-4">
              {g.subs.length > 1 ? (
                // 小类行：嵌套一层 Tabs，默认「全部」，各小类独立过滤
                <Tabs defaultValue="__all__">
                  <TabsList className="mb-3 max-w-full flex-wrap justify-start rounded-2xl">
                    <TabsTrigger value="__all__">
                      {t("packages.cat.all")}
                      <span className="text-[10.5px] tabular text-faint">{rows.length}</span>
                    </TabsTrigger>
                    {g.subs.map((s) => (
                      <TabsTrigger key={s.value} value={s.value}>
                        <s.icon className="h-3.5 w-3.5" />
                        {s.label}
                        <span className="text-[10.5px] tabular text-faint">{catCount(s.value)}</span>
                      </TabsTrigger>
                    ))}
                  </TabsList>
                  <TabsContent value="__all__">
                    <PackageRows
                      list={rows}
                      services={services}
                      runningServices={runningServices}
                      catalogById={catalogById}
                      onRefresh={refreshCatalogs}
                      onUninstallTarget={openUninstall}
                      onInstall={(target) => setInstallTarget(target)}
                      empty={t("packages.noMatches")}
                      disabled={actionsDisabled}
                      statusKnown={statusKnown}
                    />
                  </TabsContent>
                  {g.subs.map((s) => (
                    <TabsContent key={s.value} value={s.value}>
                      <PackageRows
                        list={filtered.filter((x) => x.category === s.value)}
                        services={services}
                        runningServices={runningServices}
                        catalogById={catalogById}
                        onRefresh={refreshCatalogs}
                        onUninstallTarget={openUninstall}
                        onInstall={(target) => setInstallTarget(target)}
                        empty={t("packages.noMatches")}
                        disabled={actionsDisabled}
                        statusKnown={statusKnown}
                      />
                    </TabsContent>
                  ))}
                </Tabs>
              ) : (
                <PackageRows
                  list={rows}
                  services={services}
                  runningServices={runningServices}
                  catalogById={catalogById}
                  onRefresh={refreshCatalogs}
                  onUninstallTarget={openUninstall}
                  onInstall={(target) => setInstallTarget(target)}
                  empty={t("packages.noMatches")}
                  disabled={actionsDisabled}
                  statusKnown={statusKnown}
                />
              )}
            </TabsContent>
          );
        })}
      </Tabs>}

      <ConfirmDialog
        open={!!bulkTarget}
        onOpenChange={(open) => { if (!open && !bulkRef.current) setBulkTarget(null); }}
        title={t(bulkTarget?.action === "stop" ? "packages.stopAll" : "packages.startAll")}
        description={t(bulkTarget?.action === "stop" ? "packages.bulkStopConfirm" : "packages.bulkStartConfirm")
          .replace("{count}", String(bulkTarget?.ids.length ?? 0))}
        confirmText={t(bulkReport?.failed.length ? "bulk.retryFailed" : bulkTarget?.action === "stop" ? "bulk.stop" : "bulk.start")}
        danger={bulkTarget?.action === "stop"}
        loading={bulkBusy}
        confirmDisabled={!dataReady}
        onConfirm={() => void runBulk()}
      >
        {!bulkReport && <ul className="space-y-1 text-xs text-muted [overflow-wrap:anywhere]">
          {bulkTarget?.ids.map((id) => <li key={id}>{services.find((s) => s.id === id)?.label ?? id} <span className="font-mono text-faint">{id}</span></li>)}
        </ul>}
        {readStatus}
        <BulkResult report={bulkReport} error={bulkError} services={services} busy={bulkBusy} />
      </ConfirmDialog>

      <ConfirmDialog
        open={!!uninstallTarget}
        onOpenChange={(o) => !o && !uninstallRef.current && setUninstallTarget(null)}
        onCloseAutoFocus={(event) => {
          if (uninstallOpener.current?.isConnected) {
            event.preventDefault();
            uninstallOpener.current.focus();
          }
        }}
        title={`${t("packages.uninstall")} ${uninstallTarget?.name ?? ""} ${uninstallTarget?.version ?? ""}`}
        description={t(uninstallPathPending ? "packages.uninstalledPathPending" : "packages.uninstallConfirm")}
        confirmText={t(uninstallPathPending ? "packages.retryPathCleanup" : "packages.uninstall")}
        danger={!uninstallPathPending}
        loading={uninstalling}
        confirmDisabled={!uninstallPathPending && (!dataReady || uninstallInstalling)}
        onConfirm={() => void uninstall()}
      >
        {uninstallInstalling && !uninstallPathPending && <p role="status" className="text-xs text-muted">{t("packages.installing")}</p>}
        {!uninstallPathPending && readStatus}
        {uninstallError && <div role="alert" className="rounded-lg bg-error-soft p-3 text-xs text-error [overflow-wrap:anywhere]">
          <p>{uninstallError.message}</p>
          {uninstallError.hint && <p className="mt-1">{uninstallError.hint}</p>}
        </div>}
      </ConfirmDialog>

      {/* 安装走向导弹窗：阶段时间线 + 真实进度 + 完成后可直接启动 */}
      <InstallDialog
        target={installTarget}
        onOpenChange={(o) => !o && setInstallTarget(null)}
        onDone={() => { void refreshState(true); }}
        startableAs={
          installTarget
            ? (() => {
                const g = groups.find((x) => x.id === installTarget.id);
                const v = g?.versions.find((x) => x.version === installTarget.version);
                return v?.serviceId ?? null;
              })()
            : null
        }
      />
    </div>
  );
}

/* ============ 服务条：一包一行，版本下拉点选安装/切换/启停 ============ */
function PackageRow({
  group,
  disabled,
  statusKnown,
  services,
  runningServices,
  catalog,
  onRefresh,
  onUninstall,
  onInstall,
}: {
  group: PackageGroup;
  disabled: boolean;
  statusKnown: boolean;
  services: ServiceStatus[];
  runningServices: Set<string>;
  catalog?: ReturnType<typeof useVersionCatalogs>["byId"] extends Map<string, infer Catalog> ? Catalog : never;
  onRefresh: () => Promise<void> | void;
  onUninstall: (version: string, trigger: HTMLButtonElement | null) => void;
  onInstall: (target: InstallTarget) => void;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  // 只订阅本套件的下载进度：其它包的进度事件不会让这一行重渲染
  const task = useInstallTasks((s) => activeProgressFor(s.progress, group.id));
  const cancelInstall = useInstallTasks((s) => s.cancel);
  const installTasks = useInstallTasks((s) => s.tasks);
  // PHP 扩展面板：挂在已安装且被选为「使用中」的那个版本上
  // （扩展的开关写进该版本的 php.ini，所以必须明确是哪一个版本）
  const [extVersion, setExtVersion] = React.useState<string | null>(null);

  const installedCount = group.versions.filter((v) => v.installed).length;
  const svc = group.isService;
  // 该包任一版本运行中 → 行首状态灯
  const anyRunning = group.versions.some((v) => (v.serviceId ? runningServices.has(v.serviceId) : false));
  // 已装版本之外还有更高正式版：同时检查内置与上游目录。
  const hasNewer = React.useMemo(() => {
    const installedVers = group.versions.filter((v) => v.installed).map((v) => v.version);
    if (installedVers.length === 0) return false;
    const maxInstalled = installedVers.reduce((a, b) => (cmpVersionDesc(a, b) < 0 ? a : b));
    return group.versions.some(
      (v) => !v.installed && !v.incompatible && !isPrerelease(v.version) && cmpVersionDesc(v.version, maxInstalled) < 0
    ) || (catalog?.remote ?? []).some((v) => !v.prerelease && cmpVersionDesc(v.version, maxInstalled) < 0);
  }, [group.versions, catalog]);
  /** 扩展面板作用的版本：优先「使用中」，其次任一已装版本 */
  const phpActiveVersion = React.useMemo(() => {
    const active = group.versions.find((v) => v.installed && v.active);
    const fallback = group.versions.find((v) => v.installed);
    return active?.version ?? fallback?.version ?? null;
  }, [group.versions]);

  /** 合并「清单内置版本」与「远程枚举版本」为统一下拉项。
   *  已装/已内置的优先用本地元数据；远程独有的版本标 remote。 */
  const items = React.useMemo<VersionItem[]>(() => {
    const seen = new Set<string>();
    const list: VersionItem[] = [];
    for (const v of group.versions) {
      seen.add(v.version.replace(/^[vV]/, ""));
      list.push({
        version: v.version,
        installed: v.installed,
        installing: Object.values(installTasks).some((task) => task.status === "running"
          && task.id === group.id && (!task.version || task.version === v.version)),
        active: v.active,
        running: v.installed && services.some((s) => s.id === v.serviceId
          && s.version === v.version && s.state === "running"),
        canStop: v.installed && services.some((s) => s.id === v.serviceId
          && s.version === v.version && serviceHasProcess(s)),
        transitioning: v.installed && services.some((s) => s.id === v.serviceId
          && ["starting", "stopping"].includes(s.state)),
        sizeBytes: v.sizeBytes,
        incompatible: v.incompatible,
        prerelease: isPrerelease(v.version),
      });
    }
    for (const r of catalog?.remote ?? []) {
      if (seen.has(r.version.replace(/^[vV]/, ""))) continue;
      seen.add(r.version.replace(/^[vV]/, ""));
      list.push({
        version: r.version,
        installed: false,
        installing: Object.values(installTasks).some((task) => task.status === "running"
          && task.id === group.id && (!task.version || task.version === r.version)),
        active: false,
        running: false,
        remote: r,
        sizeBytes: r.sizeBytes,
        note: r.note,
        prerelease: r.prerelease,
      });
    }
    // 版本降序：统一走 cmpVersionDesc（处理 v 前缀与预发布）
    list.sort((a, b) => cmpVersionDesc(a.version, b.version));
    return list;
  }, [group, catalog, services, installTasks]);

  const pct = task && task.total > 0 ? Math.min(100, (task.received / task.total) * 100) : 0;
  const downloading = task?.state === "downloading";
  const cancelling = useInstallTasks((s) => task ? !!s.tasks[task.taskId]?.cancelRequested : false);
  const stageLabel = task?.state === "extracting" ? t("install.stage.extract")
    : task?.state === "configuring" ? t("install.stage.config")
    : task?.state === "downloaded" || task?.state === "verifying" ? t("install.stage.verify")
    : t("install.stage.download");
  const activeVersion = task ? task.taskId.slice(group.id.length + 1) : null;

  const setDefaultVersion = async (item: VersionItem) => {
    if (disabled || item.installing) return;
    try {
      await api.setActiveVersion(group.id, item.version);
      toast.success(`${group.displayName} ${t(group.multiInstance || !group.isService ? "versions.defaultSaved" : "packages.switchedTo")} ${item.version}`);
    } finally {
      // PATH 同步失败时版本选择可能已保存，必须重新读取实际结果。
      await Promise.all(["packages", "services", "stacks", "pathenv", "databases", "db-users"].map(
        (key) => queryClient.invalidateQueries({ queryKey: [key] })
      ));
    }
  };

  const clickVersion = async (item: VersionItem) => {
    if (disabled || item.installing) return;
    const { version, installed } = item;
    const v = group.versions.find((x) => x.version === version);
    const sid = v?.serviceId ?? null;
    const isSingle = !group.multiInstance;
    const current = services.find((service) => service.id === sid);
    // 选择默认版本不触发多实例服务启停；单实例切换仍要求先停止。
    if (installed && (!sid || (isSingle && !v?.active))) {
      if (sid && current && serviceHasProcess(current)) {
        throw { code: "SERVICE_BUSY", message: `${group.displayName} ${t("packages.switchRunning")}` };
      }
      if (!item.active) await setDefaultVersion(item);
      return;
    }
    try {
      if (!installed) {
        // 安装走向导弹窗（阶段时间线 + 实时进度 + 完成后可直接启动）
        onInstall({
          id: group.id,
          displayName: group.displayName,
          version,
          sizeBytes: item.sizeBytes,
          reinstall: false,
        });
        return;
      }
      if (sid) {
        if (current && serviceHasProcess(current)) {
          await api.stopService(sid);
          toast.success(`${group.displayName} ${version} ${t("common.stopped")}`);
        } else {
          toast.info(`${t("packages.starting")} ${group.displayName} ${version}`);
          await api.startService(sid);
          toast.success(`${group.displayName} ${version} ${t("common.running")}`);
        }
      }
    } finally {
      await Promise.all(["packages", "services", "pathenv"].map(
        (key) => queryClient.invalidateQueries({ queryKey: [key] })
      ));
    }
  };

  // 不用 layout 动画：它每次渲染都要测量 DOM（强制回流），
  // 几十行的列表跟着服务轮询 / 下载进度反复重渲染时会明显卡顿
  return (
    <motion.div initial={{ opacity: 0, y: 6 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0 }}>
      <Card className="flex flex-col gap-2.5 p-3.5 transition-colors hover:border-border-strong lg:flex-row lg:items-center">
        {/* 左：图标 + 名称/描述 */}
        <div className="flex min-w-0 flex-1 items-center gap-3">
          <div className="relative">
            <div
              className={cn(
                "flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border",
                installedCount > 0 ? "border-primary/30 bg-primary-soft" : "border-border bg-card-2/60"
              )}
            >
              <ServiceIcon
                id={group.id}
                className={cn("h-[18px] w-[18px]", installedCount > 0 ? "" : "opacity-55")}
              />
            </div>
            {anyRunning && (
              <span className="absolute -right-0.5 -top-0.5 h-2.5 w-2.5 rounded-full border-2 border-card bg-running" />
            )}
          </div>
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="truncate text-[13.5px] font-medium">{group.displayName}</span>
              {hasNewer && (
                <span
                  className="rounded-full border border-info/40 px-1.5 py-px text-[9px] text-info"
                  title={t("packages.hasNewer")}
                >
                  {t("packages.hasNewer")}
                </span>
              )}
              {installedCount > 0 && (
                <Badge variant="outline" className="shrink-0 text-[10px]">
                  {installedCount}/{group.versions.length} {t("packages.installedCount")}
                </Badge>
              )}
            </div>
            <p className="truncate text-[11.5px] text-faint">
              {group.defaultPort != null ? `:${group.defaultPort} · ` : ""}
              {group.description}
            </p>
          </div>
        </div>

        {/* PHP：扩展面板入口。放在版本下拉左边，和「选版本」是同一类操作 */}
        {group.id === "php" && phpActiveVersion && (
          <PhpExtBadge version={phpActiveVersion} onOpen={() => { if (!disabled) setExtVersion(phpActiveVersion); }} />
        )}

        {/* 已装即可一键注入/移出系统 PATH —— 操作就地完成，不再绕去工具箱 */}
        {installedCount > 0 && <PathEnvToggle pkgId={group.id} disabled={disabled} />}

        {/* 右：版本下拉（清单内置 + 远程枚举的完整版本历史） */}
        <VersionPicker
          group={{ id: group.id, displayName: group.displayName, multiInstance: group.multiInstance, isService: svc }}
          items={items}
          disabled={disabled}
          statusKnown={statusKnown}
          catalog={
            catalog
              ? {
                  online: catalog.online,
                  cachedAt: catalog.cachedAt,
                  error: catalog.error,
                  loading: catalog.loading,
                }
              : undefined
          }
          onRefresh={onRefresh}
          onPick={clickVersion}
          onSetActive={setDefaultVersion}
          onUninstall={onUninstall}
        />

        {/* 下载进度（仅一条激活） */}
        {task && (
          <div className="flex w-full items-center gap-3 rounded-xl border border-info/25 bg-info-soft p-2 lg:w-72">
            <RingProgress value={pct} size={38} strokeWidth={4} indeterminate={!downloading || task.total === 0}>
              <span className="text-[9px] font-semibold tabular text-info">{downloading && task.total > 0 ? `${pct.toFixed(0)}%` : "…"}</span>
            </RingProgress>
            <div className="flex min-w-0 flex-1 flex-col gap-0.5 text-[10px]">
              <span className="truncate font-mono text-info">
                {activeVersion} · {cancelling ? t("install.cancelling") : stageLabel}
              </span>
              <span className="tabular text-faint">
                {downloading ? `${fmtBytes(task.received)}${task.total > 0 ? ` / ${fmtBytes(task.total)}` : ""} · ${fmtSpeed(task.speedBps)}` : t("install.keepOpen")}
              </span>
            </div>
            <Button
              size="sm"
              variant="ghost"
              className="h-6 px-2 text-[10px] text-faint"
              onClick={() => void cancelInstall(task.taskId)}
              disabled={cancelling || task.state === "configuring"}
            >
              {t("common.cancel")}
            </Button>
          </div>
        )}

        {!svc && group.id !== "composer" && (
          <span className="hidden text-[10px] text-faint xl:inline">{t("packages.runtimeNote")}</span>
        )}
      </Card>

      {group.id === "php" && (
        <PhpExtensionsDialog
          version={extVersion}
          open={extVersion != null}
          onOpenChange={(v) => !v && setExtVersion(null)}
        />
      )}
    </motion.div>
  );
}

/** 包组列表（大类/小类/全部 共用的渲染块） */
function PackageRows({
  list,
  disabled,
  statusKnown,
  services,
  runningServices,
  catalogById,
  onRefresh,
  onUninstallTarget,
  onInstall,
  empty,
}: {
  list: PackageGroup[];
  disabled: boolean;
  statusKnown: boolean;
  services: ServiceStatus[];
  runningServices: Set<string>;
  catalogById: ReturnType<typeof useVersionCatalogs>["byId"];
  onRefresh: (id: string) => Promise<void>;
  onUninstallTarget: (g: PackageGroup, version: string, trigger: HTMLButtonElement | null) => void;
  onInstall: (target: InstallTarget) => void;
  empty: string;
}) {
  return (
    <>
      <div className="flex flex-col gap-2.5">
        <AnimatePresence initial={false}>
          {list.map((g) => (
            <PackageRow
              key={g.id}
              group={g}
              disabled={disabled}
              statusKnown={statusKnown}
              services={services}
              runningServices={runningServices}
              catalog={catalogById.get(g.id)}
              onRefresh={() => onRefresh(g.id)}
              onUninstall={(v, trigger) => onUninstallTarget(g, v, trigger)}
              onInstall={onInstall}
            />
          ))}
        </AnimatePresence>
      </div>
      {list.length === 0 && <p className="py-12 text-center text-[13px] text-faint">{empty}</p>}
    </>
  );
}
